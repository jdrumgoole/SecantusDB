"""Record the on-disk layout of a real mongod store: the storage-model spec.

Phase 0b of ``tasks/mongod-storage-model-plan.md``. Starts a mongod (8.2.11 by
default) as a one-member replica set on a fresh directory, runs a fixed
scenario that touches every collection and index kind the plan covers, shuts
it down cleanly, and reads the directory back with a WiredTiger ``wt``
utility:

* every WiredTiger table and its creation config (``wt list -v``);
* every ``_mdb_catalog`` entry, decoded from BSON;
* every ``sizeStorer`` row;
* ``storage.bson`` and the ``WiredTiger.turtle`` version;
* every index table's raw keys and values (KeyString and TypeBits), mapped
  to its namespace and index name through the catalog.

The output JSON is the specification the later phases compare against, so
it is written deterministically: idents are replaced by
``<ns>``/``<ns>/<index>`` labels, and per-run values (UUIDs, timestamps,
ObjectIds) stay in the raw section only.

Usage (the ``wt`` utility must be a WiredTiger build with snappy, built with
``WT_STANDALONE_BUILD=OFF`` as mongod builds it; see the plan's phase 0a)::

    .venv/bin/python tools/probes/mongod_store_layout.py \\
        --wt .scratch/wt11-mdb/wt --out .scratch/layout.json

``--keep`` leaves the data directory in place for manual inspection.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as _dt
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import bson
from bson import json_util
from bson.binary import Binary
from bson.code import Code
from bson.decimal128 import Decimal128
from bson.int64 import Int64
from bson.max_key import MaxKey
from bson.min_key import MinKey
from bson.objectid import ObjectId
from bson.regex import Regex
from bson.timestamp import Timestamp
from pymongo import MongoClient

DEFAULT_MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"
WT_OPEN_CFG = "log=(enabled=true,path=journal,compressor=snappy)"

# One document per BSON type, for the KeyString corpus (index on `v`).
# Ordered by mongod's canonical type order, then by value.
KEYSTRING_VALUES: list[tuple[str, object]] = [
    ("minkey", MinKey()),
    ("null", None),
    ("int_neg", -5),
    ("int_zero", 0),
    ("int_one", 1),
    ("int_big", 2**31 - 1),
    ("long_one", Int64(1)),
    ("long_big", Int64(2**53 + 1)),
    ("double_half", 0.5),
    ("double_one", 1.0),
    ("double_neg_zero", -0.0),
    ("double_nan", float("nan")),
    ("double_inf", float("inf")),
    ("double_ninf", float("-inf")),
    ("double_big", 1e300),
    ("decimal_one", Decimal128("1")),
    ("decimal_onepointzero", Decimal128("1.0")),
    ("decimal_frac", Decimal128("0.1")),
    ("decimal_nan", Decimal128("NaN")),
    ("string_empty", ""),
    ("string_a", "a"),
    ("string_nul", "a\x00b"),
    ("string_utf8", "é漢"),
    ("object_empty", {}),
    ("object", {"x": 1, "y": "z"}),
    ("array_empty_in_obj", {"a": []}),
    ("bindata", Binary(b"\x01\x02", 0)),
    ("bindata_uuid", Binary(b"0123456789abcdef", 4)),
    ("objectid", ObjectId("650000000000000000000001")),
    ("bool_false", False),
    ("bool_true", True),
    ("date", _dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=_dt.timezone.utc)),
    ("date_neg", _dt.datetime(1960, 1, 1, tzinfo=_dt.timezone.utc)),
    ("timestamp", Timestamp(5, 6)),
    ("regex", Regex("ab+c", "i")),
    ("code", Code("function(){}")),
    ("maxkey", MaxKey()),
]


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _start_server(
    mongod: str | None, secantusd: str | None, dbpath: Path, port: int
) -> subprocess.Popen:
    log = dbpath.parent / "server.log"
    if secantusd:
        argv = [
            secantusd,
            "--storage-path",
            str(dbpath),
            "--port",
            str(port),
            "--host",
            "127.0.0.1",
        ]
    else:
        argv = [
            mongod,
            "--dbpath",
            str(dbpath),
            "--port",
            str(port),
            "--bind_ip",
            "127.0.0.1",
            "--replSet",
            "rs0",
            "--setParameter",
            "ttlMonitorEnabled=false",
            "--logpath",
            str(log),
        ]
    with open(log, "ab") as fh:
        proc = subprocess.Popen(argv, stdout=fh, stderr=subprocess.STDOUT)
    deadline = time.time() + 30
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"server exited {proc.returncode}; see {log}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return proc
        except OSError:
            time.sleep(0.2)
    proc.kill()
    raise SystemExit("server did not listen within 30s")


def _wait_primary(client: MongoClient) -> None:
    deadline = time.time() + 30
    while time.time() < deadline:
        if client.admin.command("hello").get("isWritablePrimary"):
            return
        time.sleep(0.2)
    raise SystemExit("replica set did not elect a primary")


def run_scenario(client: MongoClient, strict: bool) -> list[str]:
    """Create every collection and index kind the plan covers. Returns a log.

    ``strict`` (mongod) re-raises a failing step: the reference server must
    run the whole scenario, or the spec is incomplete. Against SecantusDB a
    failing step is recorded and the scenario carries on.
    """
    steps: list[str] = []

    def step(name: str, fn) -> None:
        try:
            fn()
        except Exception as exc:  # noqa: BLE001 - recorded, see docstring
            if strict:
                raise
            steps.append(f"FAILED {name}: {type(exc).__name__}: {str(exc)[:200]}")
            return
        steps.append(name)

    app = client["app"]

    # Plain collection, every BSON type under one index: the KeyString corpus.
    def ks():
        app.ks.insert_many(
            [{"_id": i, "label": label, "v": v} for i, (label, v) in enumerate(KEYSTRING_VALUES)]
            + [{"_id": 1000, "label": "missing"}]
        )
        app.ks.create_index("v", name="v_1")
        app.ks.create_index([("v", -1)], name="v_-1")

    step("ks: one doc per BSON type, ascending and descending index on v", ks)

    def basic():
        app.c.insert_many([{"_id": i, "a": i % 3, "b": f"s{i}", "t": [i, i + 1]} for i in range(6)])
        app.c.create_index([("a", 1), ("b", -1)], name="a_1_b_-1")
        app.c.create_index("b", unique=True, name="b_unique")
        app.c.create_index("t", name="t_multikey")
        app.c.create_index("a", name="a_partial", partialFilterExpression={"a": {"$gt": 0}})
        app.c.create_index("missing_field", sparse=True, name="sparse_idx")
        app.c.create_index("created", expireAfterSeconds=3600, name="ttl_idx")

    step("c: compound, unique, multikey, partial, sparse, TTL indexes", basic)

    def kinds():
        app.kinds.insert_many(
            [
                {
                    "_id": 1,
                    "h": "hello",
                    "loc": {"type": "Point", "coordinates": [-73.97, 40.77]},
                    "flat": [1.5, 2.5],
                    "body": "the quick brown fox",
                    "w": {"x": 1, "y": {"z": 2}},
                },
                {
                    "_id": 2,
                    "h": 42,
                    "loc": {"type": "Point", "coordinates": [2.35, 48.86]},
                    "flat": [-10, 20],
                    "body": "jumped over lazy dogs",
                    "w": {"x": "s"},
                },
            ]
        )

    step("kinds: two documents for the special index kinds", kinds)
    # One step per index kind, so a server that lacks one still builds the rest.
    for kind, spec, name in [
        ("hashed", [("h", "hashed")], "h_hashed"),
        ("2dsphere", [("loc", "2dsphere")], "loc_2dsphere"),
        ("2d", [("flat", "2d")], "flat_2d"),
        ("text", [("body", "text")], "body_text"),
        ("wildcard", [("w.$**", 1)], "w_wildcard"),
    ]:
        step(
            f"kinds: {kind} index",
            lambda spec=spec, name=name: app.kinds.create_index(spec, name=name),
        )

    def collated():
        app.create_collection("coll_ci", collation={"locale": "en", "strength": 2})
        app.coll_ci.insert_many(
            [{"_id": 1, "s": "Apple"}, {"_id": 2, "s": "apple"}, {"_id": 3, "s": "Ébène"}]
        )
        app.coll_ci.create_index("s", name="s_1")
        app.c.create_index("b", name="b_fr", collation={"locale": "fr"})

    step("coll_ci: collection default collation, collated index", collated)

    def capped():
        app.create_collection("capped", capped=True, size=4096, max=3)
        app.capped.insert_many([{"_id": i} for i in range(5)])

    step("capped: size 4096, max 3, five inserts", capped)

    def clustered():
        app.create_collection("clustered", clusteredIndex={"key": {"_id": 1}, "unique": True})
        app.clustered.insert_many(
            [
                {"_id": 1, "x": 1},
                {"_id": "s", "x": 2},
                {"_id": ObjectId("650000000000000000000002")},
            ]
        )
        app.clustered.create_index("x", name="x_1")

    step("clustered: clustered on _id, secondary index", clustered)

    def timeseries():
        app.create_collection(
            "ts", timeseries={"timeField": "t", "metaField": "m", "granularity": "seconds"}
        )
        base = _dt.datetime(2024, 1, 1, tzinfo=_dt.timezone.utc)
        app.ts.insert_many(
            [{"t": base + _dt.timedelta(seconds=i), "m": {"s": i % 2}, "v": i} for i in range(6)]
        )

    step("ts: time-series, metaField m, granularity seconds", timeseries)

    def preimages():
        app.create_collection("pre", changeStreamPreAndPostImages={"enabled": True})
        app.pre.insert_one({"_id": 1, "x": 1})
        app.pre.update_one({"_id": 1}, {"$set": {"x": 2}})

    step("pre: changeStreamPreAndPostImages, one update", preimages)

    def view():
        app.command("create", "v_view", viewOn="c", pipeline=[{"$match": {"a": 1}}])

    step("v_view: view on c", view)

    def drop_rename():
        app.todrop.insert_one({"_id": 1})
        app.todrop.create_index("x", name="x_1")
        app.todrop.drop()
        app.old.insert_one({"_id": 1})
        app.old.create_index("x", name="x_1")
        app.old.rename("renamed")

    step("todrop dropped; old renamed to renamed", drop_rename)

    def users():
        app.command(
            "createRole",
            "r1",
            privileges=[{"resource": {"db": "app", "collection": "c"}, "actions": ["find"]}],
            roles=[],
        )
        app.command("createUser", "u1", pwd="pw", roles=["r1"], mechanisms=["SCRAM-SHA-256"])

    step("users: role r1, user u1 (SCRAM-SHA-256)", users)

    def txn():
        with client.start_session() as s:
            s.with_transaction(lambda sess: app.c.insert_one({"_id": 99, "a": 9}, session=sess))

    step("txn: one committed transaction", txn)
    try:
        client.admin.command("fsync")
    except Exception as exc:  # noqa: BLE001
        if strict:
            raise
        steps.append(f"FAILED fsync: {exc}")
    return steps


def _wt(wt: str, home: Path, *args: str) -> str:
    # mongod keeps its log in journal/; today's SecantusDB store keeps it at the top.
    cfg = WT_OPEN_CFG if (home / "journal").is_dir() else "log=(enabled=true)"
    out = subprocess.run(
        [wt, "-h", str(home), "-C", cfg, *args],
        capture_output=True,
        text=True,
    )
    if out.returncode != 0:
        raise SystemExit(f"wt {' '.join(args)} failed: {out.stderr.strip()}")
    return out.stdout


def _parse_list_v(text: str) -> dict[str, str]:
    lines = text.splitlines()
    return {lines[i]: lines[i + 1] for i in range(0, len(lines) - 1, 2)}


def _dump_hex(wt: str, home: Path, uri: str) -> list[tuple[str, str]]:
    lines = _wt(wt, home, "dump", "-x", uri).splitlines()
    data = lines[lines.index("Data") + 1 :]
    return [(data[i], data[i + 1]) for i in range(0, len(data) - 1, 2)]


def _unpack_q(hexkey: str) -> int:
    """Decode WiredTiger's packed signed integer (format 'q')."""
    b = bytes.fromhex(hexkey)
    first = b[0]
    if 0x80 <= first < 0xC0:  # positive, one byte
        return first - 0x80
    if 0xC0 <= first < 0xE0:  # positive, two bytes
        return (((first & 0x1F) << 8) | b[1]) + 64
    if first >= 0xE0:  # positive, multi-byte
        n = first & 0x0F
        v = int.from_bytes(b[1 : 1 + n], "big")
        return v + 64 + 8192
    if 0x40 <= first < 0x80:  # negative, one byte
        return (first & 0x3F) - 64
    if 0x20 <= first < 0x40:  # negative, two bytes
        return (((first & 0x1F) << 8) | b[1]) - 8192 - 64
    n = 8 - (first & 0x0F)  # negative, multi-byte
    v = int.from_bytes(b[1 : 1 + n], "big") | (-(1 << (8 * n)))
    return v - 64 - 8192


def _bson(hexval: str):
    return bson.decode(bytes.fromhex(hexval), codec_options=bson.CodecOptions(tz_aware=True))


def _cfg_subset(cfg: str) -> dict[str, str]:
    """The parts of a table config that decide whether mongod can open it."""
    keys = (
        "app_metadata",
        "key_format",
        "value_format",
        "block_compressor",
        "prefix_compression",
        "log",
        "type",
        "internal_page_max",
        "leaf_page_max",
        "leaf_value_max",
        "split_pct",
        "checksum",
        "allocation_size",
        "write_timestamp_usage",
        "assert",
    )
    out: dict[str, str] = {}
    depth = 0
    start = 0
    parts = []
    for i, ch in enumerate(cfg):
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(cfg[start:i])
            start = i + 1
    parts.append(cfg[start:])
    for p in parts:
        k, _, v = p.partition("=")
        if k in keys:
            out[k] = v
    return out


def read_store(wt: str, home: Path) -> dict:
    # Read the turtle first: opening the store with `wt` rewrites it with the
    # utility's own version.
    turtle = (home / "WiredTiger.turtle").read_text().splitlines()
    meta = _parse_list_v(_wt(wt, home, "list", "-v"))
    sb = home / "storage.bson"
    storage_bson = bson.decode(sb.read_bytes()) if sb.exists() else None
    if "table:_mdb_catalog" not in meta:
        # Not a mongod-model store (today's SecantusDB layout): tables only.
        return {
            "wiredtiger_turtle_version": turtle[1] if len(turtle) > 1 else None,
            "storage_bson": storage_bson,
            "tables": [
                {
                    "uri": uri,
                    "label": uri[len("table:") :],
                    "table_config": _cfg_subset(cfg),
                    "file_config": _cfg_subset(meta.get(f"file:{uri[len('table:') :]}.wt", "")),
                }
                for uri, cfg in sorted(meta.items())
                if uri.startswith("table:")
            ],
            "catalog": None,
            "size_storer": None,
            "collections": None,
            "index_tables": None,
            "keystring_corpus": [label for label, _ in KEYSTRING_VALUES],
        }

    catalog_raw = []
    for k, v in _dump_hex(wt, home, "table:_mdb_catalog"):
        catalog_raw.append({"recordId": _unpack_q(k), "doc": _bson(v)})

    ident_label: dict[str, str] = {}
    for entry in catalog_raw:
        md = entry["doc"].get("md", {})
        ns = md.get("ns") or entry["doc"].get("ns")
        if "ident" in entry["doc"]:
            ident_label[entry["doc"]["ident"]] = ns
        for name, ident in entry["doc"].get("idxIdent", {}).items():
            ident_label[ident] = f"{ns}/{name}"

    size_storer = []
    # A SecantusDB store keeps its sizes in `secantus_size_storer` until phase
    # 6 of the plan makes them mongod's `sizeStorer`: none is recorded then.
    if "table:sizeStorer" in meta:
        for k, v in _dump_hex(wt, home, "table:sizeStorer"):
            ident = bytes.fromhex(k).rstrip(b"\x00").decode()
            size_storer.append(
                {"ident": ident, "label": ident_label.get(ident, ident), "value": _bson(v)}
            )

    tables = []
    for uri, cfg in sorted(meta.items()):
        if not uri.startswith("table:"):
            continue
        name = uri[len("table:") :]
        file_cfg = meta.get(f"file:{name}.wt", "")
        tables.append(
            {
                "uri": uri,
                "label": ident_label.get(name, name),
                "table_config": _cfg_subset(cfg),
                "file_config": _cfg_subset(file_cfg),
            }
        )

    index_dumps = {}
    for ident, label in sorted(ident_label.items(), key=lambda kv: kv[1]):
        if "/" not in label or not label.startswith("app."):
            continue
        index_dumps[label] = [
            {"key": k, "value": v} for k, v in _dump_hex(wt, home, f"table:{ident}")
        ]

    collections = {}
    for ident, label in sorted(ident_label.items(), key=lambda kv: kv[1]):
        if "/" in label:
            continue
        rows = _dump_hex(wt, home, f"table:{ident}")
        collections[label] = {
            "rows": len(rows),
            "key_sample": [k for k, _ in rows[:3]],
        }

    return {
        "wiredtiger_turtle_version": turtle[1] if len(turtle) > 1 else None,
        "storage_bson": storage_bson,
        "tables": tables,
        "catalog": catalog_raw,
        "size_storer": size_storer,
        "collections": collections,
        "index_tables": index_dumps,
        "keystring_corpus": [label for label, _ in KEYSTRING_VALUES],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--mongod", default=DEFAULT_MONGOD)
    ap.add_argument(
        "--secantusd",
        default=None,
        help="run the scenario against this secantusd-mdb binary instead of mongod (phase 0c)",
    )
    ap.add_argument(
        "--wt",
        required=True,
        help="wt utility: for a mongod store, a non-standalone build with snappy;"
        " otherwise the build that wrote the store",
    )
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument(
        "--workdir", type=Path, default=None, help="default: a temp dir under .scratch/"
    )
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    binary = args.secantusd or args.mongod
    vout = subprocess.run([binary, "--version"], capture_output=True, text=True)
    version = (vout.stdout or vout.stderr).splitlines()[0]
    scratch = Path(__file__).resolve().parents[2] / ".scratch"
    scratch.mkdir(exist_ok=True)
    work = args.workdir or Path(tempfile.mkdtemp(prefix="mongod-layout-", dir=scratch))
    dbpath = work / "db"
    dbpath.mkdir(parents=True, exist_ok=True)
    port = _free_port()
    proc = _start_server(None if args.secantusd else args.mongod, args.secantusd, dbpath, port)
    try:
        client = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        set_name = client.admin.command("hello").get("setName")
        if set_name is None:
            client.admin.command(
                "replSetInitiate",
                {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]},
            )
            set_name = "rs0"
        _wait_primary(client)
        client.close()
        # A client made before the initiate saw a standalone and refuses sessions.
        client = MongoClient(f"mongodb://127.0.0.1:{port}/?replicaSet={set_name}")
        steps = run_scenario(client, strict=args.secantusd is None)
        try:
            fcv = client.admin.command("getParameter", 1, featureCompatibilityVersion=1)[
                "featureCompatibilityVersion"
            ]
        except Exception as exc:  # noqa: BLE001 - recorded, not fatal for SecantusDB
            fcv = f"unavailable: {exc}"
        if args.secantusd:
            # secantusd-mdb has no `shutdown` command (CommandNotFound in
            # 0.5.3-beta.198); SIGTERM is its clean-close path.
            client.close()
            proc.terminate()
        else:
            # The connection drops as mongod stops.
            with contextlib.suppress(Exception):
                client.admin.command("shutdown")
        proc.wait(timeout=60)
        if proc.returncode != 0:
            raise SystemExit(f"server exited {proc.returncode} on shutdown")
        layout = read_store(args.wt, dbpath)
        layout["server_version"] = version
        layout["fcv"] = fcv
        layout["scenario"] = steps
        args.out.write_text(
            json_util.dumps(layout, indent=1, json_options=json_util.CANONICAL_JSON_OPTIONS) + "\n"
        )
        n_cat = len(layout["catalog"]) if layout["catalog"] is not None else "no"
        print(f"wrote {args.out}: {len(layout['tables'])} tables, {n_cat} catalog entries")
        for s in steps:
            if s.startswith("FAILED"):
                print(s)
    finally:
        if proc.poll() is None:
            proc.kill()
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print(f"kept {work}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

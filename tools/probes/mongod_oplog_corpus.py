"""Record what mongod 8.2.11 writes to `local.oplog.rs`, as the store holds it.

Phase 4b of ``tasks/mongod-storage-model-plan.md`` moves SecantusDB's oplog
to mongod's: the collection ``local.oplog.rs``, one entry per timestamp,
keyed by a RecordId derived from it. This probe is the specification. It
starts a one-member replica set, runs one operation of each kind the server
logs (insert, a batched insert, update, replace, delete, findAndModify, a
multi-document transaction, a retryable write, create, createIndexes,
collMod, dropIndexes, rename, drop, dropDatabase), and records, for each:

* the entries ``find`` on ``local.oplog.rs`` returns for it, in order;
* the same rows as the WiredTiger table holds them: the RecordId key (decoded
  and as the timestamp it encodes) and the stored BSON, read back with a
  ``wt`` utility after a clean shutdown.

Output: JSON ``{mongod_version, steps: [{name, entries}], table: {config,
rows: [{key_hex, record_id, ts: [secs, inc], doc}]}}``.

Usage::

    .venv/bin/python tools/probes/mongod_oplog_corpus.py --wt .scratch/wt11-mdb/wt \\
        --out tests/fixtures/storage_model/mongod-8.2.11-oplog.json
"""

from __future__ import annotations

import argparse
import contextlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import bson
from bson import json_util
from pymongo import MongoClient

DEFAULT_MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"
WT_CFG = "log=(enabled=true,path=journal,compressor=snappy)"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wt(wt: str, home: Path, *args: str) -> str:
    out = subprocess.run([wt, "-h", str(home), "-C", WT_CFG, *args], capture_output=True, text=True)
    if out.returncode:
        raise SystemExit(f"wt {args}: {out.stderr}")
    return out.stdout


def _unpack_q(hexkey: str) -> int:
    """WiredTiger's packed signed integer (format 'q')."""
    b = bytes.fromhex(hexkey)
    first = b[0]
    if 0x80 <= first < 0xC0:
        return first - 0x80
    if 0xC0 <= first < 0xE0:
        return (((first & 0x1F) << 8) | b[1]) + 64
    if first >= 0xE0:
        n = first & 0x0F
        return int.from_bytes(b[1 : 1 + n], "big") + 64 + 8192
    raise SystemExit(f"negative RecordId key {hexkey}")


def steps(client: MongoClient) -> list[tuple[str, object]]:
    db = client["app"]
    out: list[tuple[str, object]] = []

    def step(name: str, fn) -> None:
        out.append((name, fn))

    step("create", lambda: db.create_collection("c"))
    step("insert one", lambda: db.c.insert_one({"_id": 1, "a": 1}))
    step("insert many", lambda: db.c.insert_many([{"_id": i, "a": i} for i in range(2, 5)]))
    step("update one ($set)", lambda: db.c.update_one({"_id": 1}, {"$set": {"a": 10}}))
    step("update many ($inc)", lambda: db.c.update_many({"_id": {"$gte": 2}}, {"$inc": {"a": 1}}))
    step("replace one", lambda: db.c.replace_one({"_id": 2}, {"_id": 2, "b": "x"}))
    step("upsert", lambda: db.c.update_one({"_id": 9}, {"$set": {"a": 9}}, upsert=True))
    step("delete one", lambda: db.c.delete_one({"_id": 3}))
    step("delete many", lambda: db.c.delete_many({"_id": {"$gte": 4}}))
    step("findAndModify", lambda: db.c.find_one_and_update({"_id": 1}, {"$set": {"f": 1}}))

    def txn() -> None:
        with client.start_session() as s:
            s.with_transaction(
                lambda sess: (
                    db.c.insert_one({"_id": 20}, session=sess),
                    db.c.update_one({"_id": 1}, {"$set": {"t": 1}}, session=sess),
                    db.d.insert_one({"_id": 1}, session=sess),
                )
            )

    step("create d", lambda: db.create_collection("d"))
    step("transaction (insert, update, insert into another collection)", txn)

    def retryable() -> None:
        with client.start_session() as s:
            db.c.insert_one({"_id": 30}, session=s)

    step("retryable insert", retryable)
    step("createIndexes", lambda: db.c.create_index("a", name="a_1"))
    step("collMod", lambda: db.command("collMod", "c", validator={"a": {"$exists": True}}))
    step("dropIndexes", lambda: db.c.drop_index("a_1"))
    step("rename", lambda: db.c.rename("c2"))
    step("drop", lambda: db.d.drop())
    step("dropDatabase", lambda: client.drop_database("app"))
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mongod", default=DEFAULT_MONGOD)
    ap.add_argument("--wt", required=True)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    version = subprocess.run(
        [args.mongod, "--version"], capture_output=True, text=True
    ).stdout.splitlines()[0]
    scratch = Path(__file__).resolve().parents[2] / ".scratch"
    scratch.mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="oplog-corpus-", dir=scratch))
    db = work / "db"
    db.mkdir()
    port = _free_port()
    proc = subprocess.Popen(
        [
            args.mongod,
            "--dbpath",
            str(db),
            "--port",
            str(port),
            "--bind_ip",
            "127.0.0.1",
            "--replSet",
            "rs0",
            "--logpath",
            str(work / "mongod.log"),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.time() + 30
        while True:
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
                break
            except OSError as exc:
                if time.time() > deadline or proc.poll() is not None:
                    raise SystemExit("mongod did not start") from exc
                time.sleep(0.2)
        boot = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        boot.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]}
        )
        while not boot.admin.command("hello").get("isWritablePrimary"):
            time.sleep(0.2)
        boot.close()
        client = MongoClient(f"mongodb://127.0.0.1:{port}/?replicaSet=rs0")
        oplog = client["local"]["oplog.rs"]
        recorded = []
        for name, fn in steps(client):
            last = next(oplog.find().sort("$natural", -1).limit(1))["ts"]
            fn()
            time.sleep(0.2)
            entries = list(oplog.find({"ts": {"$gt": last}}).sort("$natural", 1))
            # The periodic no-op mongod writes while idle is not the step's.
            entries = [
                e
                for e in entries
                if not (e.get("op") == "n" and e.get("o", {}).get("msg") == "periodic noop")
            ]
            recorded.append({"name": name, "entries": entries})
        with contextlib.suppress(Exception):
            client.admin.command("shutdown")
        proc.wait(timeout=60)
        meta = _wt(args.wt, db, "list", "-v").splitlines()
        catalog = []
        lines = _wt(args.wt, db, "dump", "-x", "table:_mdb_catalog").splitlines()
        data = lines[lines.index("Data") + 1 :]
        for i in range(0, len(data) - 1, 2):
            catalog.append(bson.decode(bytes.fromhex(data[i + 1])))
        entry = next(c for c in catalog if c.get("ns") == "local.oplog.rs")
        uri = f"table:{entry['ident']}"
        config = meta[meta.index(f"file:{entry['ident']}.wt") + 1]
        lines = _wt(args.wt, db, "dump", "-x", uri).splitlines()
        data = lines[lines.index("Data") + 1 :]
        rows = []
        for i in range(0, len(data) - 1, 2):
            rid = _unpack_q(data[i])
            rows.append(
                {
                    "key_hex": data[i],
                    "record_id": rid,
                    "ts": [rid >> 32, rid & 0xFFFFFFFF],
                    "doc": bson.decode(bytes.fromhex(data[i + 1])),
                }
            )
        out = {
            "mongod_version": version,
            "catalog_entry": entry,
            "steps": recorded,
            "table": {"config": config, "rows": rows},
        }
        args.out.write_text(
            json_util.dumps(out, indent=1, json_options=json_util.CANONICAL_JSON_OPTIONS) + "\n"
        )
        print(f"wrote {args.out}: {len(recorded)} steps, {len(rows)} oplog rows")
    finally:
        if proc.poll() is None:
            proc.kill()
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())

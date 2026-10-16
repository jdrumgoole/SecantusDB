"""Gap tests for `tasks/backlog-rust-mongodb.md`, RM-085 to RM-112.

Each test runs the same operations against the Rust MongoDB server and
against mongod (8.2.11 on this box: standalone through the `mongod`
fixture, a single-node replica set through `mongod_rs` where the item needs
transactions or change streams) and asserts the Rust server answers what
mongod answers.
"""

from __future__ import annotations

import contextlib
import pathlib
import re
import socket
import struct
import subprocess
import time
import uuid
from collections.abc import Callable, Iterator
from typing import Any

import bson
import pymongo
import pytest
from bson import SON

from tests.backlog.conftest import RS_BINARY, mongo_answer

_RS_BANNER = re.compile(r"secantusd-rs listening on (\S+):(\d+)")


# --------------------------------------------------------------- fixtures --


@pytest.fixture(scope="module")
def mongod_rs() -> Iterator[Any]:
    """A pymongo client to a throwaway SINGLE-NODE replica set (the reference
    for transactions and change streams)."""
    from tests.test_mongod_differential import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "rm4set"))
    try:
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        host, port = _host_port(uri)
        boot.admin.command(
            {
                "replSetInitiate": {
                    "_id": "rm4set",
                    "members": [{"_id": 0, "host": f"{host}:{port}"}],
                }
            }
        )
        deadline = time.monotonic() + 60
        while not boot.admin.command("hello").get("isWritablePrimary"):
            assert time.monotonic() < deadline, "the single-node set never became primary"
            time.sleep(0.2)
        boot.close()
        client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        yield client
        client.close()
    finally:
        _stop_mongod(proc, dbpath)


def _start_rust(store: pathlib.Path, extra: list[str]) -> tuple[subprocess.Popen, Any]:
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store), *extra],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    seen: list[str] = []
    m = None
    while m is None and len(seen) < 50:
        line = proc.stdout.readline()
        if not line:
            break
        seen.append(line)
        m = _RS_BANNER.search(line)
    if not m:
        proc.kill()
        raise RuntimeError(f"secantusd-rs did not start: {seen!r}")
    client = pymongo.MongoClient(
        host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
    )
    return proc, client


def _stop_rust(proc: subprocess.Popen, client: Any) -> None:
    client.close()
    proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()


def _name(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex[:8]}"


def _both(rust: Any, ref: Any, probe: Callable[[Any], Any]) -> tuple[Any, Any]:
    return probe(rust), probe(ref)


def _drain(stream: Any, n: int, limit: float = 10.0) -> list[dict]:
    out: list[dict] = []
    deadline = time.monotonic() + limit
    while len(out) < n and time.monotonic() < deadline:
        ev = stream.try_next()
        if ev is not None:
            out.append(ev)
    return out


def _stages(plan: Any) -> list[str]:
    found: list[str] = []
    if isinstance(plan, dict):
        if "stage" in plan:
            found.append(plan["stage"])
        for v in plan.values():
            found += _stages(v)
    elif isinstance(plan, list):
        for v in plan:
            found += _stages(v)
    return found


# --------------------------------------------------- 7.072 transactions --


@pytest.mark.backlog_fixed("RM-085")
def test_transaction_change_event_carries_txn_number_and_lsid(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11: an event a transaction produced carries `txnNumber` and
    `lsid`, as the event's first two fields."""

    def probe(client: Any) -> list[str]:
        db = client[_name("rm085")]
        db.create_collection("c")
        with db.c.watch() as cs:
            with client.start_session() as s, s.start_transaction():
                db.c.insert_one({"x": 1}, session=s)
            (ev,) = _drain(cs, 1)
        return list(ev)[:2]

    got, ref = _both(rust_mongo, mongod_rs, probe)
    assert ref == ["txnNumber", "lsid"]
    assert got == ref


@pytest.mark.backlog_fixed("RM-086")
def test_two_transactions_creating_one_collection_conflict_at_commit(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11: both transactions insert into the new collection; the
    second COMMIT fails with 112."""

    def probe(client: Any) -> list[tuple]:
        db = client[_name("rm086")]
        db.command("ping")
        s1, s2 = client.start_session(), client.start_session()
        try:
            s1.start_transaction()
            s2.start_transaction()
            a = mongo_answer(lambda: bool(db.c.insert_one({"t": 1}, session=s1).acknowledged))
            b = mongo_answer(lambda: bool(db.c.insert_one({"t": 2}, session=s2).acknowledged))
            c1 = mongo_answer(lambda: s2.commit_transaction())
            c2 = mongo_answer(lambda: s1.commit_transaction())
            return [a, b, c1, c2[:2]]
        finally:
            for s in (s1, s2):
                if s.in_transaction:
                    with contextlib.suppress(pymongo.errors.PyMongoError):
                        s.abort_transaction()
                s.end_session()

    got, ref = _both(rust_mongo, mongod_rs, probe)
    assert ref == [("ok", True), ("ok", True), ("ok", None), ("err", 112)]
    assert got == ref


@pytest.mark.backlog("RM-087")
def test_fast_count_sees_another_sessions_uncommitted_insert(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11: a `count` with no query answers 3 for 2 committed
    documents and one inserted by an open transaction."""

    def probe(client: Any) -> int:
        db = client[_name("rm087")]
        db.c.insert_many([{"a": 1}, {"a": 2}])
        with client.start_session() as s:
            s.start_transaction()
            db.c.insert_one({"a": 3}, session=s)
            n = db.command("count", "c")["n"]
            s.abort_transaction()
        return n

    got, ref = _both(rust_mongo, mongod_rs, probe)
    assert ref == 3
    assert got == ref


@pytest.mark.backlog_fixed("RM-087")
def test_find_refuses_an_unknown_field(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `find` with a field it does not know answers 40415."""

    def probe(client: Any) -> Any:
        db = client[_name("rm087")]
        ans = mongo_answer(lambda: db.command(SON([("find", "c"), ("bogusField", 1)])))
        return ans[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ("err", 40415)
    assert got == ref


# --------------------------------------------------- 7.071 time series --


@pytest.mark.backlog("RM-088")
def test_bucket_collection_can_be_deleted_from(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: a delete on `system.buckets.<name>` succeeds and removes
    the measurements (the Rust server answers 115)."""

    def probe(client: Any) -> tuple:
        db = client[_name("rm088")]
        db.create_collection("ts", timeseries={"timeField": "t"})
        db.ts.insert_many([{"t": bson.datetime.datetime(2024, 1, 1), "v": i} for i in range(3)])
        ans = mongo_answer(lambda: db["system.buckets.ts"].delete_many({}).acknowledged)
        return ans[:2], db.ts.count_documents({})

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == (("ok", True), 0)
    assert got == ref


def _placed(client: Any, batches: list) -> tuple:
    """Insert `batches` (lists of `(minute, v)`; `v` None for no `v`) into a
    fresh time-series collection: the buckets' counts and the scan order."""
    db = client[_name("rm088")]
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    start = bson.datetime.datetime(2026, 1, 1)
    n = 0
    for batch in batches:
        docs = []
        for minute, v in batch:
            d = {"_id": n, "t": start + bson.datetime.timedelta(minutes=minute), "m": "a"}
            if v is not None:
                d["v"] = v
            docs.append(d)
            n += 1
        db.ts.insert_many(docs)
    counts = [b["control"]["count"] for b in db["system.buckets.ts"].find()]
    return counts, [d["_id"] for d in db.ts.find({}, {"_id": 1})]


@pytest.mark.backlog_fixed("RM-088")
def test_a_batch_is_placed_in_time_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sorts an insert batch by time before bucketing it: a
    batch that runs backwards shares one bucket, where the same two
    measurements inserted one at a time take two."""
    got, ref = _both(
        rust_mongo,
        mongod,
        lambda c: (_placed(c, [[(30, 1), (-10, 1)]]), _placed(c, [[(30, 1)], [(-10, 1)]])),
    )
    assert ref == (([2], [1, 0]), ([1, 1], [1, 0]))
    assert got == ref


@pytest.mark.backlog_fixed("RM-088")
def test_a_field_changing_type_rolls_the_bucket_over(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 rolls a bucket over when a field changes type (a number
    to a string and back); a later measurement before the current bucket's
    window is reopened into the EARLIEST bucket whose window holds it."""
    got, ref = _both(
        rust_mongo,
        mongod,
        lambda c: _placed(c, [[(30, 1)], [(31, "s")], [(32, 3)], [(31.5, None)]]),
    )
    assert ref == ([2, 1, 1], [0, 3, 1, 2])
    assert got == ref


@pytest.mark.backlog("RM-089")
def test_measurement_without_id_is_stored_without_one(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: a measurement inserted with no `_id` reads back with none."""

    def probe(client: Any) -> list:
        db = client[_name("rm089")]
        db.create_collection("ts", timeseries={"timeField": "t"})
        db.command("insert", "ts", documents=[{"t": bson.datetime.datetime(2024, 1, 1), "v": 1}])
        return [sorted(d) for d in db.ts.find({})]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == [["t", "v"]]
    assert got == ref


# --------------------------------------------- convertToCapped & friends --


@pytest.mark.backlog_fixed("RM-090")
def test_compact_refuses_an_unknown_field(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (standalone): `compact` with a field it does not know
    answers 40415."""

    def probe(client: Any) -> Any:
        db = client[_name("rm090")]
        db.c.insert_one({"a": 1})
        return mongo_answer(lambda: db.command(SON([("compact", "c"), ("bogusField", 1)])))[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ("err", 40415)
    assert got == ref


# ------------------------------------------------------- 7.070 collation --


@pytest.mark.backlog("RM-091")
def test_canonically_equivalent_strings_differ_without_normalization(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11, `normalization: false` (the default): the two orderings
    of a dot below and a diaeresis on `o` are not equal."""
    a = "ọ̈"  # o-dot-below, then diaeresis
    b = "ọ̈"  # o-diaeresis, then dot below

    def probe(client: Any) -> int:
        db = client[_name("rm091")]
        db.c.insert_one({"s": a})
        return db.c.count_documents({"s": b}, collation={"locale": "en"})

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == 0
    assert got == ref


@pytest.mark.backlog("RM-092")
def test_collated_duplicate_document_id_shows_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: a duplicate under a collated `_id` whose value is a
    document reports the string inside it as its sort key (`"W\\x01\\x05"`
    for `"x"` under `en`, strength 2), not as written."""

    def probe(client: Any) -> Any:
        db = client[_name("rm092")]
        db.create_collection("c", collation={"locale": "en", "strength": 2})
        db.c.insert_one({"_id": {"a": "X"}})
        try:
            db.c.insert_one({"_id": {"a": "x"}})
        except pymongo.errors.DuplicateKeyError as e:
            return (e.details or {}).get("keyValue", {}).get("_id", {}).get("a")
        return "no error"

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == "W\x01\x05"
    assert got not in ("x", "X", "no error")


@pytest.mark.backlog_fixed("RM-093")
def test_out_change_stream_sees_rename_then_invalidate(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a change stream on `$out`'s target sees `rename` then
    `invalidate`."""

    def probe(client: Any) -> list[str]:
        db = client[_name("rm093")]
        db.src.insert_one({"a": 1})
        db.dst.insert_one({"b": 1})
        with db.dst.watch() as cs:
            list(db.src.aggregate([{"$out": "dst"}]))
            evs = _drain(cs, 2)
        return [e["operationType"] for e in evs]

    got, ref = _both(rust_mongo, mongod_rs, probe)
    assert ref == ["rename", "invalidate"]
    assert got == ref


@pytest.mark.backlog("RM-095")
def test_five_collation_types_are_accepted(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 accepts each of these `@collation=` types."""
    locales = [
        "ko@collation=search",
        "ko@collation=searchjl",
        "zh@collation=big5han",
        "zh@collation=gb2312han",
        "ln@collation=phonetic",
    ]

    def probe(client: Any) -> list:
        db = client[_name("rm095")]
        db.c.insert_one({"s": "a"})
        return [
            mongo_answer(lambda loc=loc: db.c.count_documents({}, collation={"locale": loc}))[:2]
            for loc in locales
        ]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == [("ok", 1)] * 5
    assert got == ref


@pytest.mark.backlog_fixed("RM-096")
def test_did_you_mean_for_a_three_letter_code(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: the error for locale `eng` (its wording, with `Did you
    mean`)."""

    def probe(client: Any) -> Any:
        db = client[_name("rm096")]
        return mongo_answer(lambda: db.c.count_documents({}, collation={"locale": "eng"}))

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref[0] == "err"
    assert got == ref


@pytest.mark.backlog_fixed("RM-096")
def test_lookup_from_a_view_with_another_collation(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$lookup` from a view whose collation differs from the
    pipeline's answers 167."""

    def probe(client: Any) -> Any:
        db = client[_name("rm096")]
        db.base.insert_one({"a": 1})
        db.local.insert_one({"a": 1})
        db.command("create", "v", viewOn="base", pipeline=[], collation={"locale": "fr"})
        pipe = [{"$lookup": {"from": "v", "localField": "a", "foreignField": "a", "as": "j"}}]
        return mongo_answer(lambda: list(db.local.aggregate(pipe)) and "rows")[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ("err", 167)
    assert got == ref


@pytest.mark.backlog_fixed("RM-097")
def test_densify_full_partitions_fill_the_whole_range(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `bounds: "full"` with `partitionByFields` fills every
    partition across the collection's range, with mongod's repeat of `(c,1)`."""

    def probe(client: Any) -> list:
        db = client[_name("rm097")]
        db.c.insert_many([{"p": p, "v": v} for p, v in [("a", 0), ("c", 1), ("a", 2), ("b", 3)]])
        pipe = [
            {
                "$densify": {
                    "field": "v",
                    "partitionByFields": ["p"],
                    "range": {"step": 1, "bounds": "full"},
                }
            },
            {"$project": {"_id": 0}},
        ]
        return sorted((d["p"], d["v"]) for d in db.c.aggregate(pipe))

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref


@pytest.mark.backlog("RM-157")
def test_densify_partition_bounds_emit_in_partition_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (2026-10-16): `$densify` with `bounds: "partition"`
    sorts by the partition fields and then the field, so the partitions come
    out in key order; this server emits them in the order first seen."""

    def probe(client: Any) -> list:
        db = client[_name("rm157")]
        db.c.insert_many(
            [{"p": p, "v": v} for p, v in [("c", 0), ("a", 3), ("c", 2), ("b", 1), ("a", 0)]]
        )
        pipe = [
            {
                "$densify": {
                    "field": "v",
                    "partitionByFields": ["p"],
                    "range": {"step": 1, "bounds": "partition"},
                }
            },
            {"$project": {"_id": 0}},
        ]
        return [(d["p"], d["v"]) for d in db.c.aggregate(pipe)]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == [("a", 0), ("a", 1), ("a", 2), ("a", 3), ("b", 1), ("c", 0), ("c", 1), ("c", 2)]
    assert got == ref


@pytest.mark.backlog_fixed("RM-098")
def test_set_window_fields_partition_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$setWindowFields` emits its partitions in
    partition-key order."""

    def probe(client: Any) -> list:
        db = client[_name("rm098")]
        db.c.insert_many([{"p": "c", "v": 1}, {"p": "a", "v": 2}, {"p": "b", "v": 3}])
        pipe = [
            {
                "$setWindowFields": {
                    "partitionBy": "$p",
                    "sortBy": {"v": 1},
                    "output": {"n": {"$count": {}}},
                }
            }
        ]
        return [d["p"] for d in db.c.aggregate(pipe)]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ["a", "b", "c"]
    assert got == ref


# ------------------------------------------------- 7.068 bulkWrite / TTL --


@pytest.mark.backlog_fixed("RM-100")
def test_clustered_collection_has_no_separate_id_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `createIndexes` on a clustered collection answers
    `numIndexesBefore: 0`."""

    def probe(client: Any) -> Any:
        db = client[_name("rm100")]
        db.create_collection("c", clusteredIndex={"key": {"_id": 1}, "unique": True})
        r = db.command("createIndexes", "c", indexes=[{"key": {"a": 1}, "name": "a_1"}])
        return r["numIndexesBefore"], r["numIndexesAfter"]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == (0, 1)
    assert got == ref


@pytest.mark.backlog_fixed("RM-101")
def test_pipeline_upsert_with_array_id(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: a pipeline upsert whose result has an array `_id`
    answers 54."""

    def probe(client: Any) -> Any:
        db = client[_name("rm101")]
        return mongo_answer(
            lambda: db.c.update_one({"x": 1}, [{"$set": {"_id": [1, 2]}}], upsert=True).raw_result
        )[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ("err", 54)
    assert got == ref


@pytest.mark.backlog_fixed("RM-103")
def test_ttl_monitor_sleep_secs_reports_the_started_cadence(
    tmp_path: pathlib.Path,
) -> None:
    """mongod 8.2.11 started with `--setParameter ttlMonitorSleepSecs=7`
    reads back 7; the Rust server started with a 7-second sweep should too."""
    cfg = tmp_path / "secantusd.toml"
    cfg.write_text("[storage]\nttl_sweep_seconds = 7.0\n")
    proc, client = _start_rust(tmp_path / "store", ["--config", str(cfg)])
    try:
        got = client.admin.command("getParameter", 1, ttlMonitorSleepSecs=1)
    finally:
        _stop_rust(proc, client)
    assert got.get("ttlMonitorSleepSecs") == 7


@pytest.mark.backlog_fixed("RM-105")
def test_bulk_write_collection_uuid_mismatch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `bulkWrite` with a `collectionUUID` that does not match
    reports 361 for the op."""

    def probe(client: Any) -> Any:
        dbn = _name("rm105")
        client[dbn].c.insert_one({"a": 1})
        cmd = SON(
            [
                ("bulkWrite", 1),
                ("ops", [{"insert": 0, "document": {"b": 1}}]),
                (
                    "nsInfo",
                    [{"ns": f"{dbn}.c", "collectionUUID": bson.Binary.from_uuid(uuid.uuid4())}],
                ),
            ]
        )
        ans = mongo_answer(lambda: client.admin.command(cmd))
        if ans[0] == "err":
            return ans[:2]
        r = ans[1]
        return ("ok", r["nErrors"], [d.get("code") for d in r["cursor"]["firstBatch"]])

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref


def _raw_command(client: Any, db: str, body: dict) -> dict:
    """Send `body` as a bare OP_MSG (pymongo rewrites `bulkWrite`'s `ops`)."""
    host, port = client.address
    doc = bson.encode(SON([*body.items(), ("$db", db)]))
    payload = struct.pack("<I", 0) + b"\x00" + doc
    msg = struct.pack("<iiii", 16 + len(payload), 1, 0, 2013) + payload
    with socket.create_connection((host, port), timeout=10) as sock:
        sock.sendall(msg)
        head = b""
        while len(head) < 16:
            head += sock.recv(16 - len(head))
        (length,) = struct.unpack("<i", head[:4])
        rest = b""
        while len(rest) < length - 16:
            rest += sock.recv(length - 16 - len(rest))
    return bson.decode(rest[5:])


@pytest.mark.backlog_fixed("RM-105")
def test_bulk_write_ops_not_an_array(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `bulkWrite` whose `ops` is not an array is refused."""

    def probe(client: Any) -> Any:
        dbn = _name("rm105")
        client[dbn].command("ping")
        r = _raw_command(
            client, "admin", SON([("bulkWrite", 1), ("ops", 5), ("nsInfo", [{"ns": f"{dbn}.c"}])])
        )
        return r.get("ok"), r.get("code"), r.get("errmsg")

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref[0] == 0
    assert got == ref


@pytest.mark.backlog_fixed("RM-105")
def test_bulk_write_ns_info_not_an_array(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `bulkWrite` whose `nsInfo` is not an array is refused."""

    def probe(client: Any) -> Any:
        r = _raw_command(
            client,
            "admin",
            SON([("bulkWrite", 1), ("ops", [{"insert": 0, "document": {}}]), ("nsInfo", "x")]),
        )
        return r.get("ok"), r.get("code"), r.get("errmsg")

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref[0] == 0
    assert got == ref


# ------------------------------------------------- 7.067 index management --


@pytest.mark.backlog_fixed("RM-081")
def test_list_indexes_in_creation_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists indexes in creation order."""

    def probe(client: Any) -> list:
        db = client[_name("rm107")]
        db.c.create_index("z")
        db.c.create_index("b")
        db.c.create_index("m")
        return [i["name"] for i in db.c.list_indexes()]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ["_id_", "z_1", "b_1", "m_1"]
    assert got == ref


# ------------------------------------------------------------ 7.066 views --


@pytest.mark.backlog_fixed("RM-108")
def test_hint_on_a_view_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: a hint naming no index, on a view, answers 2."""

    def probe(client: Any) -> Any:
        db = client[_name("rm108")]
        db.base.insert_one({"a": 1})
        db.command("create", "v", viewOn="base", pipeline=[])
        return mongo_answer(lambda: list(db.v.find({}).hint("nope_1")))[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == ("err", 2)
    assert got == ref


@pytest.mark.backlog_fixed("RM-108")
def test_index_stats_on_a_view_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$indexStats` on a view WITH a pipeline answers 40602
    (the view's stages go first, so it is no longer the first stage). On a
    view with an empty pipeline it reports the base collection's indexes."""

    def probe(client: Any) -> Any:
        db = client[_name("rm108")]
        db.base.insert_one({"a": 1})
        db.base.create_index("a")
        db.command("create", "v", viewOn="base", pipeline=[{"$match": {}}])
        db.command("create", "v0", viewOn="base", pipeline=[])
        refused = mongo_answer(lambda: list(db.v.aggregate([{"$indexStats": {}}])))[:2]
        names = sorted(d["name"] for d in db.v0.aggregate([{"$indexStats": {}}]))
        return refused, names

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == (("err", 40602), ["_id_", "a_1"])
    assert got == ref


@pytest.mark.backlog_fixed("RM-108")
def test_hint_on_a_view_count_distinct_and_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `count` and `distinct` on a view refuse a hint naming no
    index (2), and a `find` on a view returns documents in the hinted
    index's order, `$natural: -1` reversed."""

    def probe(client: Any) -> Any:
        db = client[_name("rm108")]
        db.base.insert_many([{"_id": i, "a": v} for i, v in enumerate([3, 1, 2])])
        db.base.create_index("a")
        db.command("create", "v", viewOn="base", pipeline=[])
        return (
            mongo_answer(lambda: db.command("count", "v", hint="nope_1"))[:2],
            mongo_answer(lambda: db.command("distinct", "v", key="a", hint="nope_1"))[:2],
            [d["_id"] for d in db.v.find({}).hint("a_1")],
            [d["_id"] for d in db.v.find({}).hint([("$natural", -1)])],
        )

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref == (("err", 2), ("err", 2), [1, 2, 0], [2, 1, 0])
    assert got == ref


@pytest.mark.backlog_fixed("RM-108")
def test_create_existing_view_with_another_pipeline_wording(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: re-creating a view with another pipeline answers 48 in
    its own words."""
    dbn = _name("rm108")

    def probe(client: Any) -> Any:
        db = client[dbn]
        db.base.insert_one({"a": 1})
        db.command("create", "v", viewOn="base", pipeline=[])
        return mongo_answer(
            lambda: db.command("create", "v", viewOn="base", pipeline=[{"$match": {"a": 1}}])
        )

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref[:2] == ("err", 48)
    assert got == ref


# --------------------------------------------------------- 7.065 capped --


@pytest.mark.backlog_fixed("RM-110")
def test_coll_mod_capped_size_event_names_fields_as_sent(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a `showExpandedEvents` change stream sees a `collMod` of
    `cappedSize` with the field named as sent."""

    def probe(client: Any) -> Any:
        db = client[_name("rm110")]
        db.create_collection("c", capped=True, size=4096)
        with db.watch(show_expanded_events=True) as cs:
            db.command("collMod", "c", cappedSize=8192)
            (ev,) = _drain(cs, 1)
        return ev["operationType"], sorted(ev.get("operationDescription", {}))

    got, ref = _both(rust_mongo, mongod_rs, probe)
    assert got == ref


@pytest.mark.backlog("RM-096")
def test_validator_reads_the_collections_collation(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 checks a collation-sensitive validator under the
    COLLECTION's collation, whatever collation the update statement names:
    `{k: "A"}` under `en` strength 2 accepts `k: "a"` for a statement with
    the simple collation too. The Rust server used the statement's (121)."""

    def probe(client: Any) -> list:
        db = client[_name("rm096v")]
        db.create_collection("v", collation={"locale": "en", "strength": 2}, validator={"k": "A"})
        db.v.insert_one({"_id": 1, "k": "A"})
        out = []
        for coll in (None, {"locale": "simple"}, {"locale": "fr", "strength": 3}):
            out.append(
                mongo_answer(
                    lambda c=coll: (
                        db.v.update_one(
                            {"_id": 1}, {"$set": {"k": "a"}}, collation=c
                        ).modified_count
                    )
                )[:2]
            )
            db.v.update_one({"_id": 1}, {"$set": {"k": "A"}}, bypass_document_validation=True)
        return out

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref


@pytest.mark.backlog_fixed("RM-179")
def test_union_with_documents_needs_no_collection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$unionWith: {pipeline: [{$documents: [...]}]}`
    with no `coll`; the Rust server refused any `$unionWith` without one (9)."""

    def probe(client: Any) -> Any:
        db = client[_name("rm179")]
        db.c.insert_one({"_id": 1})
        stage = {"$unionWith": {"pipeline": [{"$documents": [{"x": 1}]}]}}
        return mongo_answer(lambda: list(db.c.aggregate([stage, {"$project": {"_id": 0}}])))[:2]

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref


@pytest.mark.backlog_fixed("RM-180")
def test_lookup_from_a_db_and_coll_document(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses `$lookup` with `from: {db, coll}` naming an
    ordinary collection with 9 ("$lookup with syntax {from: {db:<>,
    coll:<>},..} is not supported for db: ..."); the Rust server answered 2
    ("$lookup requires 'from' (string)")."""

    def probe(client: Any) -> Any:
        db = client[_name("rm180")]
        db.c.insert_one({"_id": 1})
        stage = {
            "$lookup": {
                "from": {"db": db.name, "coll": "c"},
                "localField": "_id",
                "foreignField": "_id",
                "as": "j",
            }
        }
        got = mongo_answer(lambda: list(db.c.aggregate([stage])))
        return got[:2] + (str(got[2]).replace(db.name, "@db"),) if got[0] == "err" else got

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref

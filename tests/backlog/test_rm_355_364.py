"""What the `Slow query` line and profile entry still leave out after RM-023
(2026-10-27), and the regression tests of what RM-023 put in.

Each gap test runs one operation on both servers with every operation
logged and profiled (`tests/backlog/opdebug.py`) and compares the one thing
its item names with mongod 8.2's.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from pymongo.write_concern import WriteConcern

from tests.backlog.opdebug import fresh, logged, no_timed_yields, seed


def _both(
    rust_mongo: Any, mongod_rs: Any, op: Any, setup: Any = seed, name: str | None = None
) -> dict:
    return {
        "rust": logged(rust_mongo, False, setup, op, name=name),
        "mongod": logged(mongod_rs, True, setup, op, name=name),
    }


def _line(lines: list, command: str) -> dict:
    for _, attr in lines:
        cmd = attr.get("command", {})
        if next(iter(cmd), "") == command:
            return attr
    raise AssertionError(f"no line for {command}: {lines}")


# ------------------------------------------------------------- RM-023 --


_COUNTED = (
    "keysExamined",
    "docsExamined",
    "hasSortStage",
    "nMatched",
    "nBatches",
    "nModified",
    "ninserted",
    "ndeleted",
    "nUpserted",
    "cursorExhausted",
    "keysInserted",
    "keysDeleted",
    "nreturned",
)


def _counts(attr: dict) -> dict:
    return {k: attr[k] for k in _COUNTED if k in attr}


_READS = {
    "count": lambda d: d.command("count", "c", query={"x": {"$gt": 3}}),
    "distinct": lambda d: d.c.distinct("y", {"x": {"$lt": 7}}),
    "aggregate": lambda d: list(
        d.c.aggregate([{"$match": {"x": {"$gte": 4}}}, {"$group": {"_id": "$y", "n": {"$sum": 1}}}])
    ),
    "find": lambda d: list(d.c.find({"z": 0}).sort("y", 1)),
}


@pytest.mark.backlog_fixed("RM-023")
def test_a_read_that_does_not_stream_counts_what_it_examined(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line and profile entry for a `count` through a
    COUNT_SCAN (9 keys for 8 matches: the scan counts the call that finds
    its end), a `distinct`, an `aggregate` read whole for a `$group` and a
    `find` with a blocking sort carry the keys and documents its plan
    examined (measured 2026-10-27). This server's had none."""
    answers = {}
    for name, op in _READS.items():
        got = _both(rust_mongo, mongod_rs, op)
        answers[name] = {
            server: (_counts(_line(lines, name)), [_counts(e) for e in entries])
            for server, (lines, entries) in got.items()
        }
    assert answers["count"]["mongod"][0]["keysExamined"] == 9
    for name, a in answers.items():
        assert a["rust"] == a["mongod"], name


_WRITES = {
    "update_one": lambda d: d.c.update_one({"x": {"$gte": 5}}, {"$set": {"x": 55}}),
    "upsert": lambda d: d.c.update_one({"x": 999}, {"$set": {"y": 9}}, upsert=True),
    "delete_many": lambda d: d.c.delete_many({"z": 0}),
    "findAndModify": lambda d: d.c.find_one_and_update({"x": 3}, {"$set": {"q": 1}}),
    "insert": lambda d: (
        d.c.create_index([("y", 1), ("z", 1)]),
        d.c.insert_one({"x": [1, 2, 3], "y": 1, "z": [4, 5]}),
    ),
}


@pytest.mark.backlog_fixed("RM-023")
def test_a_write_counts_what_it_examined_and_the_keys_it_wrote(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for each update and delete statement, and for
    an insert and a `findAndModify`, carries what the statement's plan
    examined, its plan and cache key, and the index keys it inserted and
    deleted -- an update's both, an upsert's inserted, a delete's deleted,
    a multikey insert one per key (6 for `{x: [1, 2, 3], y: 1, z: [4, 5]}`
    over `_id`, `x` and `{y, z}`). Measured 2026-10-27."""
    fields = (*_COUNTED, "planSummary", "planCacheShapeHash", "planCacheKey")
    answers = {}
    for name, op in _WRITES.items():
        got = _both(rust_mongo, mongod_rs, op)
        answers[name] = {
            server: [
                (c, attr.get("type"), {k: attr[k] for k in fields if k in attr})
                for c, attr in lines
                if next(iter(attr.get("command", {})), "") != "createIndexes"
            ]
            for server, (lines, _) in got.items()
        }
    assert answers["insert"]["mongod"][-1][2]["keysInserted"] == 6
    for name, a in answers.items():
        assert a["rust"] == a["mongod"], name


@pytest.mark.backlog_fixed("RM-023")
def test_a_line_names_its_engine_concerns_and_collection(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` and for an insert under `{w: 1}`:
    `isFromUserConnection`, `collectionType`, `queryFramework`, the read
    concern and the write concern with their provenance, and the
    `remote` and `protocol`, each in its place (measured 2026-10-27)."""
    ops = {
        "find": lambda d: list(d.c.find({"x": 1})),
        "insert": lambda d: d.get_collection("c", write_concern=WriteConcern(w=1)).insert_one(
            {"_id": 500}
        ),
    }
    shown = (
        "isFromUserConnection",
        "collectionType",
        "queryFramework",
        "readConcern",
        "writeConcern",
        "protocol",
    )
    leave_out = {
        # RM-355, RM-358, RM-360, RM-357 and the times.
        "queues",
        "queryShapeHash",
        "numInterruptChecks",
        "locks",
        "flowControl",
        "storage",
    }
    for name, op in ops.items():
        got = _both(rust_mongo, mongod_rs, op)
        lines = {s: _line(lines, name) for s, (lines, _) in got.items()}
        values = {s: {k: a.get(k) for k in shown} for s, a in lines.items()}
        order = {s: [k for k in a if k not in leave_out] for s, a in lines.items()}
        assert values["rust"] == values["mongod"], name
        assert order["rust"] == order["mongod"], name
        assert isinstance(lines["rust"].get("remote"), str), name


@pytest.mark.backlog_fixed("RM-023")
def test_storage_says_what_the_engine_read(tmp_path: Any) -> None:
    """A read whose pages are not in the cache says so in its line's
    `storage`: `data.bytesRead` and `data.timeReadingMicros`, as mongod's
    does, from WiredTiger's per-session statistics for the sessions the
    operation used. Read here from a collection a restart left on disk
    only (a server of the test's own)."""
    import json
    import subprocess

    import pymongo

    from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output

    def start() -> tuple[subprocess.Popen, Any]:
        proc = subprocess.Popen(
            [str(RS_BINARY), "--port", "0", "--storage-path", str(tmp_path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        assert proc.stdout is not None
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        drain_output(proc)
        return proc, pymongo.MongoClient(host=m.group(1), port=int(m.group(2)))

    proc, client = start()
    try:
        client.s.big.insert_many([{"i": i, "b": "x" * 50_000} for i in range(200)])
        client.close()
        proc.terminate()
        proc.wait(60)
        proc, client = start()
        client.s.command("profile", 0, slowms=0)
        list(client.s.big.find({"i": {"$lt": 100}}))
        client.s.command("profile", 0, slowms=100)
        lines = [
            json.loads(line)["attr"]
            for line in client.admin.command("getLog", "global")["log"]
            if "Slow query" in line
        ]
        finds = [a for a in lines if a.get("ns") == "s.big" and "find" in a.get("command", {})]
        assert finds, "no line for the find"
        data = finds[-1].get("storage", {}).get("data", {})
        assert data.get("bytesRead", 0) > 0, finds[-1]
        assert isinstance(data.get("timeReadingMicros"), int), finds[-1]
    finally:
        client.close()
        proc.terminate()
        proc.wait(60)


# ------------------------------------------------------------- RM-355 --


@pytest.mark.backlog("RM-355")
def test_a_line_says_what_execution_control_admitted(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for every command carries `queues` (the
    ingress and execution admissions it took) and its `storage` section
    `timeWaitingMicros.storageExecutionMicros`, the time it waited for
    execution admission, so a `find` always has `storage` (measured
    2026-10-27). This server has no execution admission control."""
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.find({"x": {"$gt": 1}})))
    answers = {}
    for server, (lines, _) in got.items():
        attr = _line(lines, "find")
        waiting = attr.get("storage", {}).get("timeWaitingMicros", {})
        answers[server] = (
            sorted(attr.get("queues", {})),
            "storageExecutionMicros" in waiting,
        )
    assert answers["mongod"] == (["execution", "ingress"], True)
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-356 --


@pytest.mark.backlog_fixed("RM-356")
def test_explain_counts_the_end_of_a_count_scan(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's COUNT_SCAN counts the call that finds its end as a
    key examined whether or not a key lies past its bounds: over keys 0 to
    11, `{x: {$gt: 3}}` examines 9 and `{x: 99}` 1 (measured 2026-10-27).
    This server's `explain` gave 8 and 0."""
    out = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client["rm356"]
        db.drop_collection("c")
        seed(db)
        out[server] = [
            db.command("explain", {"count": "c", "query": q}, verbosity="executionStats")[
                "executionStats"
            ]["totalKeysExamined"]
            for q in ({"x": {"$gt": 3}}, {"x": {"$gte": 3}}, {"x": {"$lt": 4}}, {"x": 99})
        ]
    assert out["mongod"] == [9, 10, 5, 1]
    assert out["rust"] == out["mongod"]


# ------------------------------------------------------------- RM-357 --


def _plain(d: Any) -> Any:
    """`d` through a client of the same server that sends no retryable
    writes: no `txnNumber`, so no session is checked out and no session row
    is kept."""
    client = pymongo.MongoClient(*d.client.address, directConnection=True, retryWrites=False)
    _PLAIN.append(client)
    return client[d.name]


_PLAIN: list = []


def _w1(d: Any) -> Any:
    return d.get_collection("c", write_concern=WriteConcern(w=1))


def _write_locks(rust_mongo: Any, mongod_rs: Any, op: Any) -> dict:
    """Each server's `locks` and `flowControl` on each line `op` wrote."""
    try:
        got = _both(rust_mongo, mongod_rs, op)
    finally:
        while _PLAIN:
            _PLAIN.pop().close()
    return {
        server: [
            (next(iter(a.get("command", {})), ""), a.get("locks"), a.get("flowControl"))
            for _, a in lines
            # `$out` lists the collections on mongod: lines of their own.
            if next(iter(a.get("command", {})), "") != "listCollections"
        ]
        for server, (lines, _) in got.items()
    }


#: Writes whose lines carry mongod's locks and flow control tickets: a
#: retryable write's session checkout and session row, the journal wait of
#: `j: true` and `w: "majority"`, a ticket per write round taken apart
#: (measured 8.2.11, 2026-11-07).
_MATCHING_WRITES = {
    "insert": lambda d: d.c.insert_one({"_id": 100}),
    "insert with w 1": lambda d: _w1(d).insert_one({"_id": 100}),
    "insert with j": lambda d: d.get_collection(
        "c", write_concern=WriteConcern(w=1, j=True)
    ).insert_one({"_id": 100}),
    "insert of three": lambda d: d.c.insert_many([{"_id": 100}, {"_id": 101}, {"_id": 102}]),
    "insert creating its collection": lambda d: d.n.insert_one({"_id": 1}),
    "insert not retryable": lambda d: _plain(d).c.insert_one({"_id": 100}),
    "insert not retryable with w 1": lambda d: _w1(_plain(d)).insert_one({"_id": 100}),
    "findAndModify not retryable": lambda d: _w1(_plain(d)).find_one_and_update(
        {"_id": 1}, {"$set": {"q": 1}}
    ),
    "drop": lambda d: d.command("drop", "c", writeConcern={"w": 1}),
    "create": lambda d: d.command("create", "n", writeConcern={"w": 1}),
}


@pytest.mark.backlog_fixed("RM-357")
@pytest.mark.parametrize("name", list(_MATCHING_WRITES))
def test_a_write_reports_mongods_locks_and_flow_control(
    rust_mongo: Any, mongod_rs: Any, name: str
) -> None:
    """mongod 8.2.11's line for a retryable insert under the default
    `w: "majority"` takes the replication state transition lock four times
    and the global lock in IX twice (`{ReplicationStateTransition: {w: 4},
    Global: {r: 1, w: 2}, ...}`) and counts `flowControl: {acquireCount:
    1}`: the session's checkout, its row in `config.transactions` written
    under the insert's locks, and the wait for the journal (measured
    2026-10-27 and 2026-11-07). This server took one round of each and had
    no flow control."""
    answers = _write_locks(rust_mongo, mongod_rs, _MATCHING_WRITES[name])
    assert answers["mongod"]
    assert answers["rust"] == answers["mongod"]


#: Writes whose locks are not mongod's yet (RM-357's remainder).
_DIVERGING_WRITES = {
    # A write that writes nothing reads the latest optime for the client's
    # last op under the global IS: twice for an insert, update or delete
    # (the write path and the command), once for another command.
    "insert of a duplicate key": lambda d: _w1(d).insert_one({"_id": 1}),
    "delete matching nothing": lambda d: _w1(d).delete_one({"_id": 999}),
    "create of a collection there": lambda d: d.command("create", "c", writeConcern={"w": 1}),
    # Each statement keeps its session row under its own locks, and takes
    # its round as it runs: this server takes every statement's first.
    "update": lambda d: _w1(d).update_one({"_id": 1}, {"$set": {"q": 1}}),
    "update of two statements": lambda d: _plain(d).command(
        "update",
        "c",
        updates=[
            {"q": {"_id": 1}, "u": {"$set": {"k": 1}}},
            {"q": {"_id": 2}, "u": {"$set": {"k": 1}}},
        ],
        writeConcern={"w": 1},
    ),
    # The image of a retryable findAndModify is written under its locks;
    # one that removes takes one round.
    "findAndModify": lambda d: _w1(d).find_one_and_update({"_id": 1}, {"$set": {"q": 1}}),
    "findAndModify that removes": lambda d: _w1(_plain(d)).find_one_and_delete({"_id": 1}),
    "collMod": lambda d: d.command("collMod", "c", writeConcern={"w": 1}),
    "$out": lambda d: list(_w1(_plain(d)).aggregate([{"$match": {}}, {"$out": "o"}])),
    # A statement of a transaction prints `{}`; its entry has the
    # transaction's locks.
    "insert in a transaction": lambda d: _in_transaction(d),
    "aggregate of $documents": lambda d: list(d.aggregate([{"$documents": [{"a": 1}]}])),
}


def _in_transaction(d: Any) -> None:
    with d.client.start_session() as s, s.start_transaction():
        d.c.insert_one({"_id": 200}, session=s)


@pytest.mark.backlog("RM-357")
@pytest.mark.parametrize("name", list(_DIVERGING_WRITES))
def test_more_writes_report_mongods_locks(rust_mongo: Any, mongod_rs: Any, name: str) -> None:
    """mongod 8.2.11's `locks` and `flowControl` on the lines of the writes
    above (measured 2026-11-07), which this server's lock rounds do not
    take yet."""
    answers = _write_locks(rust_mongo, mongod_rs, _DIVERGING_WRITES[name])
    assert answers["mongod"]
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-358 --


@pytest.mark.backlog_fixed("RM-358")
def test_a_read_names_its_query_shape(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line and profile entry for a `find`, a `distinct` and
    an `aggregate` carry `queryShapeHash`, the SHA-256 of the query's
    representative shape (measured 2026-10-27), which covers the namespace:
    both servers run it in a database of the same name. This server
    computed no query shape."""
    reads = {
        "find": lambda d: list(d.c.find({"x": {"$gt": 1}})),
        "distinct": lambda d: d.c.distinct("y", {"x": {"$lt": 7}}),
        "aggregate": lambda d: list(d.c.aggregate([{"$match": {"x": {"$gte": 4}}}])),
    }
    answers = {}
    for command, op in reads.items():
        got = _both(rust_mongo, mongod_rs, op, name=fresh("qshape"))
        answers[command] = {
            s: (
                _line(lines, command).get("queryShapeHash"),
                [e.get("queryShapeHash") for e in entries],
            )
            for s, (lines, entries) in got.items()
        }
    for command, both in answers.items():
        assert both["mongod"][0], command
        assert both["mongod"][1] == [both["mongod"][0]], command
    ours = {c: b["rust"] for c, b in answers.items()}
    assert ours == {c: b["mongod"] for c, b in answers.items()}


_UNSHAPED = {
    "a geo predicate": {
        "find": "c",
        "filter": {"loc": {"$geoWithin": {"$center": [[0, 0], 5]}}},
    },
    "an expression outside the nary operators": {
        "aggregate": "c",
        "pipeline": [{"$project": {"d": {"$dateToString": {"date": "$t", "format": "%Y"}}}}],
        "cursor": {},
    },
    "a stage outside the shaped ones": {
        "aggregate": "c",
        "pipeline": [{"$facet": {"a": [{"$match": {"x": 1}}]}}],
        "cursor": {},
    },
    "a $lookup with a pipeline": {
        "aggregate": "c",
        "pipeline": [{"$lookup": {"from": "c", "pipeline": [{"$match": {"x": 1}}], "as": "j"}}],
        "cursor": {},
    },
}


@pytest.mark.backlog("RM-358")
def test_a_query_of_every_kind_names_its_query_shape(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's `explain` (as its line and profile entry) carries a
    `queryShapeHash` for every `find`, `distinct` and `aggregate` it parses
    (measured 2026-11-07). This server shapes the common operators and
    stages (`tests/mongod_live/test_query_shape_hash.py`) and prints none
    for the rest: geo and text predicates, `$where`, `$jsonSchema`, the
    aggregation operators with a serialization of their own, and stages
    beyond the shaped list."""
    name = fresh("qshape")
    answers = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        db = client[name]
        db.c.insert_one({"x": 1, "loc": [0, 0]})
        answers[server] = {
            what: db.command("explain", cmd, verbosity="queryPlanner").get("queryShapeHash")
            for what, cmd in _UNSHAPED.items()
        }
        client.drop_database(name)
    assert all(answers["mongod"].values()), answers["mongod"]
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-359 --


@pytest.mark.backlog_fixed("RM-359")
def test_a_profile_entry_carries_the_execution_stats(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's profile entry for a `find` carries `execStats`, the
    plan's stages with what each did (`FETCH` over `IXSCAN`, measured
    2026-10-27). This server's entry had none."""

    def stages(stats: Any) -> list:
        out = []
        while isinstance(stats, dict):
            out.append(stats.get("stage"))
            stats = stats.get("inputStage")
        return out

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.find({"x": {"$gt": 1}})))
    answers = {s: [stages(e.get("execStats")) for e in entries] for s, (_, entries) in got.items()}
    assert answers["mongod"] == [["FETCH", "IXSCAN"]]
    assert answers["rust"] == answers["mongod"]


def _timeless(stats: Any) -> Any:
    """`execStats` with each stage's time as its type: a coarse clock's
    milliseconds, which two servers do not share."""
    if isinstance(stats, dict):
        return {
            k: (type(v).__name__ if k == "executionTimeMillisEstimate" else _timeless(v))
            for k, v in stats.items()
        }
    if isinstance(stats, list):
        return [_timeless(v) for v in stats]
    return stats


def _two_indexes(d: Any) -> None:
    seed(d)
    d.c.create_index([("y", 1), ("x", 1)])


def _merge_only(d: Any) -> None:
    d.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    d.c.create_index([("y", 1), ("x", 1)])


def _many(d: Any) -> None:
    d.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(45)])
    d.c.create_index("x")


#: Operations whose stages this server runs as mongod's plan runs them, each
#: with its setup: mongod 8.2.11's profile entries for each (a `getMore`'s
#: too) carry the same `execStats`, stage by stage and count by count
#: (measured 2026-11-07).
_RUNS: dict[str, tuple[Any, Any]] = {
    "find_ixscan": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}))),
    "find_collscan": (seed, lambda d: list(d.c.find({"z": 0}))),
    "find_all": (seed, lambda d: list(d.c.find({}))),
    "find_none": (seed, lambda d: list(d.c.find({"x": {"$gt": 100}}))),
    "find_id": (seed, lambda d: list(d.c.find({"_id": 3}))),
    "find_id_in": (seed, lambda d: list(d.c.find({"_id": {"$in": [1, 3, 5]}}))),
    "find_sort": (seed, lambda d: list(d.c.find({"z": 0}).sort("y", 1))),
    "find_sort_limit": (seed, lambda d: list(d.c.find({"z": 0}).sort("y", 1).limit(2))),
    "find_sort_batches": (seed, lambda d: list(d.c.find({"z": 0}).sort("y", 1).batch_size(2))),
    "find_backward": (seed, lambda d: list(d.c.find({"x": {"$gt": 6}}).sort("x", -1))),
    "find_limit": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}).limit(3))),
    "find_limit_is_batch": (
        seed,
        lambda d: list(d.c.find({"x": {"$gt": 1}}).limit(3).batch_size(3)),
    ),
    "find_limit_in_get_more": (
        seed,
        lambda d: list(d.c.find({"x": {"$gt": 1}}).limit(5).batch_size(2)),
    ),
    "find_batches": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}).batch_size(2))),
    "find_full_batch": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}).batch_size(10))),
    "find_collscan_batches": (seed, lambda d: list(d.c.find({"z": 0}).batch_size(2))),
    "find_skip": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}).skip(2))),
    "find_skip_limit": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}).skip(2).limit(3))),
    "find_skip_filtered": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}, "z": 0}).skip(1))),
    "find_covered": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}}, {"x": 1, "_id": 0}))),
    "find_projected": (seed, lambda d: list(d.c.find({"x": {"$gt": 6}}, {"y": 1}))),
    "find_or": (_two_indexes, lambda d: list(d.c.find({"$or": [{"x": {"$lt": 3}}, {"y": 2}]}))),
    "find_or_batches": (
        _two_indexes,
        lambda d: list(d.c.find({"$or": [{"x": {"$lt": 3}}, {"y": 2}]}).batch_size(2)),
    ),
    "find_sort_merge": (
        _two_indexes,
        lambda d: list(d.c.find({"y": {"$in": [0, 1]}}).sort("x", 1)),
    ),
    "find_sort_merge_batches": (
        _two_indexes,
        lambda d: list(d.c.find({"y": {"$in": [0, 1]}}).sort("x", 1).batch_size(3)),
    ),
    "count": (seed, lambda d: d.command("count", "c", query={"x": {"$gt": 3}})),
    "count_skip_limit": (
        seed,
        lambda d: d.command("count", "c", query={"x": {"$gt": 1}}, skip=2, limit=3),
    ),
    "count_fetch_limit": (
        seed,
        lambda d: d.command("count", "c", query={"x": {"$gt": 1}, "z": 0}, limit=2),
    ),
    "count_collscan": (seed, lambda d: d.command("count", "c", query={"z": 1})),
    "count_records": (seed, lambda d: d.command("count", "c", skip=3, limit=4)),
    "distinct": (seed, lambda d: d.c.distinct("y", {"x": {"$lt": 7}})),
    "update_many": (seed, lambda d: d.c.update_many({"x": {"$gt": 8}}, {"$set": {"w": 1}})),
    "update_many_some_unchanged": (
        seed,
        lambda d: d.c.update_many({"x": {"$gt": 8}}, {"$set": {"z": 0}}),
    ),
    "update_many_collscan": (seed, lambda d: d.c.update_many({"z": 1}, {"$set": {"w": 1}})),
    "update_many_none": (seed, lambda d: d.c.update_many({"x": {"$gt": 80}}, {"$set": {"w": 1}})),
    "update_one": (seed, lambda d: d.c.update_one({"x": 5}, {"$set": {"w": 1}})),
    "update_one_collscan": (seed, lambda d: d.c.update_one({"z": 1, "y": 2}, {"$set": {"w": 1}})),
    "update_one_none": (seed, lambda d: d.c.update_one({"x": 80}, {"$set": {"w": 1}})),
    "upsert": (seed, lambda d: d.c.update_one({"x": 80}, {"$set": {"w": 1}}, upsert=True)),
    "update_by_id": (seed, lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}})),
    "update_by_id_hinted": (
        seed,
        lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}}, hint="_id_"),
    ),
    "delete_by_id": (seed, lambda d: d.c.delete_one({"_id": 3})),
    "delete_by_id_none": (seed, lambda d: d.c.delete_one({"_id": 300})),
    "find_and_update_by_id": (
        seed,
        lambda d: d.c.find_one_and_update({"_id": 3}, {"$set": {"w": 1}}),
    ),
    "find_and_delete_by_id": (seed, lambda d: d.c.find_one_and_delete({"_id": 3})),
    "delete_many": (seed, lambda d: d.c.delete_many({"x": {"$lt": 2}})),
    "delete_many_batches": (_many, lambda d: d.c.delete_many({"x": {"$gte": 0}})),
    "delete_many_none": (seed, lambda d: d.c.delete_many({"x": {"$gt": 50}})),
    "delete_one": (seed, lambda d: d.c.delete_one({"x": {"$gt": 5}})),
    "delete_one_collscan": (seed, lambda d: d.c.delete_one({"z": 1, "y": 2})),
    "find_and_update": (seed, lambda d: d.c.find_one_and_update({"x": 4}, {"$set": {"w": 1}})),
    "find_and_delete": (seed, lambda d: d.c.find_one_and_delete({"x": {"$gt": 5}})),
    "find_and_update_none": (
        seed,
        lambda d: d.c.find_one_and_update({"x": 50}, {"$set": {"w": 1}}),
    ),
    "find_and_upsert": (
        seed,
        lambda d: d.c.find_one_and_update({"x": 77}, {"$set": {"w": 1}}, upsert=True),
    ),
}


@pytest.fixture
def steady_mongod(mongod_rs: Any) -> Iterator[Any]:
    """The reference with its time-based plan yields off
    (`opdebug.no_timed_yields`), which would add a save and a restore to
    every stage of a run slowed by load."""
    with no_timed_yields(mongod_rs):
        yield mongod_rs


@pytest.mark.backlog_fixed("RM-359")
@pytest.mark.parametrize("name", list(_RUNS))
def test_a_profile_entry_reports_each_stage_as_it_ran(
    rust_mongo: Any, steady_mongod: Any, name: str
) -> None:
    """mongod 8.2.11's profile entry for a read, each batch of its cursor,
    a `count`, a `distinct`, each update and delete statement and a
    `findAndModify` carries `execStats`: every stage's works, results, the
    works that needed more time, its saves and restores, whether it is at
    its end, and what it examined -- counted as the plan ran, not
    re-derived (a FETCH over a SORT_MERGE works once more than the merge;
    `find`'s LIMIT works once past its last result and a `getMore`'s does
    not; a write's input yields around each document it writes, a
    BATCHED_DELETE's once per ten). Measured 2026-11-07; this server's
    entries had none."""
    setup, op = _RUNS[name]
    got = _both(rust_mongo, steady_mongod, op, setup)
    answers = {
        s: [_timeless(e.get("execStats")) for e in entries] for s, (_, entries) in got.items()
    }
    assert answers["mongod"] and all(answers["mongod"]), answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-359")
def test_a_sort_of_index_keys_reports_its_works(rust_mongo: Any, steady_mongod: Any) -> None:
    """mongod 8.2.11's FETCH over a SORT of index keys (`type: "default"`)
    over an IXSCAN: the SORT works each key in, once per result out and
    once to find its end, by batch (measured 2026-11-07). Its
    `totalDataSizeSorted` is left out: this server sorts the keys without
    its sorter and counts none (RM-375)."""

    def sizeless(stats: Any) -> Any:
        if isinstance(stats, dict):
            return {k: sizeless(v) for k, v in stats.items() if k != "totalDataSizeSorted"}
        return stats

    got = _both(
        rust_mongo,
        steady_mongod,
        lambda d: list(d.c.find({"y": {"$gte": 1}}).sort("x", 1).batch_size(3)),
        _merge_only,
    )
    answers = {
        s: [sizeless(_timeless(e.get("execStats"))) for e in entries]
        for s, (_, entries) in got.items()
    }
    assert [e["inputStage"]["stage"] for e in answers["mongod"]] == ["SORT"] * 3
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-359")
def test_an_aggregate_or_an_insert_has_no_execution_stats(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 gives an `aggregate`'s and an insert's profile entry no
    `execStats` (measured 2026-11-07)."""
    ops = {
        "aggregate": lambda d: list(d.c.aggregate([{"$match": {"x": {"$gte": 4}}}])),
        "insert": lambda d: d.c.insert_one({"_id": 100}),
    }
    for name, op in ops.items():
        got = _both(rust_mongo, mongod_rs, op)
        answers = {s: ["execStats" in e for e in entries] for s, (_, entries) in got.items()}
        assert answers["mongod"] == [False], name
        assert answers["rust"] == answers["mongod"], name


# ------------------------------------------------------------- RM-360 --


@pytest.mark.backlog("RM-360")
def test_a_line_counts_its_interrupt_checks(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a command carries `numInterruptChecks`,
    how many times it checked whether it had been killed (5 for a `find`
    of ten documents through an index, measured 2026-10-27)."""
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.find({"x": {"$gt": 1}})))
    answers = {s: "numInterruptChecks" in _line(lines, "find") for s, (lines, _) in got.items()}
    assert answers["mongod"] is True
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-361 --


@pytest.mark.backlog_fixed("RM-361")
def test_a_write_says_how_long_it_held_its_oplog_slot(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line and profile entry for an insert carry
    `totalOplogSlotDurationMicros`: how long the write held its oplog
    timestamp before committing (measured 2026-10-27). This server's had
    none; its storage now times each minted run of oplog seqs from its
    mint to its transaction's commit."""
    got = _both(rust_mongo, mongod_rs, lambda d: d.c.insert_one({"_id": 100}))
    answers = {
        s: ("totalOplogSlotDurationMicros" in _line(lines, "insert"))
        for s, (lines, _) in got.items()
    }
    assert answers["mongod"] is True
    assert answers["rust"] == answers["mongod"]


@pytest.fixture(scope="module")
def rust_mongo_async(tmp_path_factory: pytest.TempPathFactory) -> Iterator[Any]:
    """A `secantusd-rs` of this module's own, on the asynchronous oplog.

    `--data-logged` as well: a store whose data tables are not logged (the
    default) stamps each write with its oplog timestamp, so it keeps the
    synchronous oplog and says so at start ("the asynchronous oplog is not
    used ..."); without it this fixture would test the synchronous one."""
    import subprocess

    from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output

    assert RS_BINARY.exists(), f"{RS_BINARY} is not built (./inv rust-binary-build)"
    store = tmp_path_factory.mktemp("rust-mongo-async")
    proc = subprocess.Popen(
        [
            str(RS_BINARY),
            "--port",
            "0",
            "--storage-path",
            str(store),
            "--oplog-async",
            "--data-logged",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs --oplog-async did not start: {line!r}"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
        )
        yield client
        client.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()


@pytest.mark.backlog("RM-379")
def test_an_async_oplog_write_says_how_long_it_held_its_oplog_slot(
    rust_mongo_async: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for an insert carries
    `totalOplogSlotDurationMicros` (RM-361). On the asynchronous oplog
    (`--oplog-async`) this server mints a write's entries only after the
    write commits, so no slot is open inside it and the line has no field."""
    got = _both(rust_mongo_async, mongod_rs, lambda d: d.c.insert_one({"_id": 100}))
    answers = {
        s: ("totalOplogSlotDurationMicros" in _line(lines, "insert"))
        for s, (lines, _) in got.items()
    }
    assert answers["mongod"] is True
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-362 --


@pytest.mark.backlog_fixed("RM-362")
def test_a_read_of_a_view_names_the_view_it_resolved(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` of a view carries
    `resolvedViews`: the view, its chain of namespaces and the pipeline it
    resolved to (measured 2026-10-27). This server resolved the view and
    printed nothing of it."""

    def setup(db: Any) -> None:
        seed(db)
        db.command("create", "v", viewOn="c", pipeline=[])

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.v.find({"x": 1})), setup)
    answers = {}
    for server, (lines, _) in got.items():
        resolved = _line(lines, "find").get("resolvedViews")
        answers[server] = [
            (r.get("dependencyChain"), r.get("resolvedPipeline")) for r in resolved or []
        ]
    assert answers["mongod"] == [(["v", "c"], [])]
    assert answers["rust"] == answers["mongod"]


def _views_setup(db: Any) -> None:
    seed(db)
    db.d.insert_many([{"_id": n, "k": n} for n in range(3)])
    db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])
    db.command("create", "w", viewOn="v", pipeline=[{"$project": {"y": 0}}])
    db.command("create", "vd", viewOn="d", pipeline=[{"$match": {"k": {"$gte": 0}}}])
    db.command(
        "create",
        "vl",
        viewOn="c",
        pipeline=[{"$lookup": {"from": "vd", "localField": "x", "foreignField": "k", "as": "j"}}],
    )
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.create_collection("tsg", timeseries={"timeField": "t", "granularity": "minutes"})
    db.command("create", "vts", viewOn="ts", pipeline=[{"$match": {"m": 1}}])


_VIEW_READS = {
    "nested view": lambda d: list(d.w.find()),
    "aggregate of a view": lambda d: list(d.w.aggregate([{"$match": {"x": 2}}])),
    "count of a view": lambda d: d.command("count", "v", query={}),
    "distinct of a view": lambda d: d.v.distinct("y"),
    "a view's own $lookup": lambda d: list(d.vl.find()),
    "time-series": lambda d: list(d.ts.find()),
    "time-series by granularity": lambda d: list(d.tsg.find()),
    "view of a time-series": lambda d: list(d.vts.find()),
    "$lookup, $unionWith, $facet": lambda d: list(
        d.w.aggregate(
            [
                {"$lookup": {"from": "vd", "localField": "x", "foreignField": "k", "as": "j"}},
                {"$unionWith": {"coll": "d", "pipeline": [{"$unionWith": "vts"}]}},
                {"$facet": {"a": [{"$lookup": {"from": "vl", "pipeline": [], "as": "q"}}]}},
            ]
        )
    ),
    "$graphLookup": lambda d: list(
        d.c.aggregate(
            [
                {
                    "$graphLookup": {
                        "from": "vd",
                        "startWith": "$x",
                        "connectFromField": "k",
                        "connectToField": "k",
                        "as": "j",
                    }
                }
            ]
        )
    ),
    "$merge target named in another database": lambda d: (
        list(d.c.aggregate([{"$merge": {"into": {"db": d.name + "_other", "coll": "v"}}}])),
        d.client.drop_database(d.name + "_other"),
    ),
    "explain": lambda d: d.command("explain", {"find": "v", "filter": {}}),
    "getMore": lambda d: list(d.v.find(batch_size=2)),
    "a read that fails as it runs": lambda d: list(
        d.v.find({"$expr": {"$divide": [1, {"$subtract": ["$x", "$x"]}]}})
    ),
    "no view": lambda d: list(
        d.c.aggregate([{"$lookup": {"from": "nope", "pipeline": [], "as": "j"}}])
    ),
}


@pytest.mark.backlog_fixed("RM-362")
def test_every_view_a_read_resolves_is_named(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2's line and profile entry name every view among the
    namespaces a read reads -- its own, its stages' (`$lookup`,
    `$graphLookup`, `$unionWith`, `$facet`, a `$merge` target, looked up by
    its name in the command's database), and those of the views' own
    pipelines -- by name, each with its chain down to its collection and the
    pipeline it resolved to (a time-series collection's is its
    `$_internalUnpackBucket`). A `getMore` names none, a read that fails
    once it ran still does. Measured 8.2.1, 2026-11-07. (Where the field
    sits is `opdebug::LINE_ORDER` / `PROFILE_ORDER`'s.)"""
    answers = {}
    for case, op in _VIEW_READS.items():
        got = _both(rust_mongo, mongod_rs, op, _views_setup)
        answers[case] = {}
        for server, (lines, entries) in got.items():
            seen = []
            for attrs in [a for _, a in lines] + entries:
                views = attrs.get("resolvedViews")
                seen.append(
                    (
                        next(iter(attrs.get("command", {})), ""),
                        [
                            (
                                v["viewNamespace"].split(".", 1)[1],
                                v["dependencyChain"],
                                v["resolvedPipeline"],
                            )
                            for v in views or []
                        ],
                    )
                )
            answers[case][server] = seen
    assert answers["nested view"]["mongod"][0][1] == [
        ("w", ["w", "v", "c"], [{"$match": {"x": {"$gte": 1}}}, {"$project": {"y": 0}}])
    ]
    assert answers["time-series by granularity"]["mongod"][0][1] == [
        (
            "tsg",
            ["tsg", "system.buckets.tsg"],
            [{"$_internalUnpackBucket": {"timeField": "t", "bucketMaxSpanSeconds": 86400}}],
        )
    ]
    for case, a in answers.items():
        assert a["rust"] == a["mongod"], case


# ------------------------------------------------------------- RM-363 --


@pytest.mark.backlog("RM-363")
def test_a_contested_read_counts_what_it_examined(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` two indexes could serve counts what
    the plan it chose by trial examined. This server reads such a plan
    through its store's router, and counts nothing."""

    def setup(db: Any) -> None:
        seed(db)
        db.c.create_index("y")

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.find({"x": {"$gte": 2}, "y": 1})), setup)
    answers = {s: _counts(_line(lines, "find")) for s, (lines, _) in got.items()}
    assert "keysExamined" in answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-363")
def test_a_read_of_a_time_series_collection_counts_what_it_examined(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a `find` of a time-series collection counts
    the buckets it examined (`keysExamined: 0, docsExamined: 0` over none,
    measured 2026-10-27). This server reads it whole, and counts nothing."""

    def setup(db: Any) -> None:
        db.create_collection("ts", timeseries={"timeField": "t"})

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.ts.find()), setup)
    answers = {s: _counts(_line(lines, "find")) for s, (lines, _) in got.items()}
    assert answers["mongod"].get("docsExamined") == 0
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-363")
def test_creating_a_view_inserts_its_definition(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 creates a view by inserting its definition into
    `system.views`, and its `create` line counts that key
    (`keysInserted: 1`, measured 2026-10-27). This server keeps a view in
    its catalog and inserts nothing."""
    got = _both(
        rust_mongo,
        mongod_rs,
        lambda d: d.command("create", "v", viewOn="c", pipeline=[]),
    )
    answers = {s: _line(lines, "create").get("keysInserted") for s, (lines, _) in got.items()}
    assert answers["mongod"] == 1
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-364 --


@pytest.mark.backlog("RM-364")
def test_a_transaction_statement_counts_its_updates(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for an insert inside a transaction counts, in
    `storage.data`, the dirty bytes and the updates the transaction holds
    so far (`txnBytesDirty`, `txnNumUpdates`, measured 2026-10-27). The
    WiredTiger this server vendors (mongodb-7.0.33) keeps the first and
    not the second."""

    def op(db: Any) -> None:
        with db.client.start_session() as s, s.start_transaction():
            db.c.insert_one({"_id": 700}, session=s)

    got = _both(rust_mongo, mongod_rs, op)
    answers = {
        s: sorted(_line(lines, "insert").get("storage", {}).get("data", {}))
        for s, (lines, _) in got.items()
    }
    assert answers["mongod"] == ["txnBytesDirty", "txnNumUpdates"]
    assert answers["rust"] == answers["mongod"]

"""What the `Slow query` line and profile entry still leave out after RM-023
(2026-10-27), and the regression tests of what RM-023 put in.

Each gap test runs one operation on both servers with every operation
logged and profiled (`tests/backlog/opdebug.py`) and compares the one thing
its item names with mongod 8.2's.
"""

from __future__ import annotations

from typing import Any

import pymongo
import pytest
from pymongo.write_concern import WriteConcern

from tests.backlog.opdebug import fresh, logged, seed


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
        # RM-355, RM-358, RM-360, RM-361, RM-357 and the times.
        "queues",
        "queryShapeHash",
        "numInterruptChecks",
        "totalOplogSlotDurationMicros",
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


@pytest.mark.backlog("RM-359")
def test_a_profile_entry_carries_the_execution_stats(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's profile entry for a `find` carries `execStats`, the
    plan's stages with what each did (`FETCH` over `IXSCAN`, measured
    2026-10-27). This server's entry has none."""

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


@pytest.mark.backlog("RM-361")
def test_a_write_says_how_long_it_held_its_oplog_slot(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line and profile entry for an insert carry
    `totalOplogSlotDurationMicros`: how long the write held its oplog
    timestamp before committing (measured 2026-10-27)."""
    got = _both(rust_mongo, mongod_rs, lambda d: d.c.insert_one({"_id": 100}))
    answers = {
        s: ("totalOplogSlotDurationMicros" in _line(lines, "insert"))
        for s, (lines, _) in got.items()
    }
    assert answers["mongod"] is True
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------------------- RM-362 --


@pytest.mark.backlog("RM-362")
def test_a_read_of_a_view_names_the_view_it_resolved(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` of a view carries
    `resolvedViews`: the view, its chain of namespaces and the pipeline it
    resolved to (measured 2026-10-27)."""

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

"""What the `Slow query` line and profile entry still leave out after RM-023
(2026-10-27), and the regression tests of what RM-023 put in.

Each gap test runs one operation on both servers with every operation
logged and profiled (`tests/backlog/opdebug.py`) and compares the one thing
its item names with mongod 8.2's.
"""

from __future__ import annotations

import datetime
import json
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


@pytest.mark.backlog_fixed("RM-355")
def test_a_line_says_what_execution_control_admitted(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for every command carries `queues` (the
    ingress and execution admissions it took) and its `storage` section
    `timeWaitingMicros.storageExecutionMicros`, the time it waited for
    execution admission, so a `find` always has `storage` (measured
    2026-10-27). This server's line printed neither until 2026-11-09; the
    counts are compared operation by operation in
    `tests/mongod_live/test_slow_query_lines.py`."""
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


@pytest.mark.backlog_fixed("RM-355")
def test_admission_control_is_reported_where_mongod_reports_it(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's `serverStatus.queues` has the connection limiter's
    `ingressSessionEstablishment` beside `execution` and `ingress`, and a
    `$currentOp` row has the operation's `queues` (each pool's
    `admissions`, `totalTimeQueuedMicros`, `isHoldingTicket`) and its
    `currentQueue` (measured 2026-11-09). This server's had neither until
    2026-11-12; the values are compared in
    `tests/mongod_live/test_ingress_admission.py`."""
    answers = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        status = client.admin.command("serverStatus")["queues"]
        rows = list(
            client.admin.aggregate([{"$currentOp": {}}, {"$match": {"command.aggregate": 1}}])
        )
        assert len(rows) == 1, server
        answers[server] = (
            sorted(status),
            {pool: sorted(q) for pool, q in rows[0].get("queues", {}).items()},
            "currentQueue" in rows[0],
        )
    pool = ["admissions", "isHoldingTicket", "totalTimeQueuedMicros"]
    assert answers["mongod"] == (
        ["execution", "ingress", "ingressSessionEstablishment"],
        {"ingress": pool, "execution": pool},
        True,
    )
    assert answers["rust"] == answers["mongod"]


def _transaction_lines(client: Any, is_mongod: bool) -> dict:
    """The `queues` of the `Slow query` lines of a transaction committed and
    of one aborted, each of one insert."""
    db = client[fresh("txn")]
    db.create_collection("c")
    last = client.admin.command("getLog", "global")["log"][-1]
    client.admin.command("profile", 0, slowms=-1 if is_mongod else 0)
    try:
        for verb in ("commit", "abort"):
            with client.start_session() as s:
                s.start_transaction()
                db.c.insert_one({"v": verb}, session=s)
                getattr(s, f"{verb}_transaction")()
    finally:
        client.admin.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    log = log[log.index(last) + 1 :]
    client.drop_database(db.name)
    lines = {}
    for raw in log:
        entry = json.loads(raw)
        command = next(iter(entry.get("attr", {}).get("command", {})), "")
        if entry.get("msg") == "Slow query" and command.endswith("Transaction"):
            lines[command] = entry["attr"].get("queues")
    return lines


@pytest.mark.backlog("RM-355")
def test_ending_a_transaction_takes_mongods_execution_tickets(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 unstashes a transaction's resources to end it, and its
    `abortTransaction` line counts three execution admissions, as its
    `commitTransaction` line does; this server's abort counts two (measured
    2026-11-12)."""
    got = {
        "rust": _transaction_lines(rust_mongo, False),
        "mongod": _transaction_lines(mongod_rs, True),
    }
    assert got["mongod"]["abortTransaction"]["execution"] == {"admissions": 3}
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-550")
def test_the_connection_rate_limiter_has_its_bypass_list(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 exempts the clients in
    `ingressConnectionEstablishmentRateLimiterBypass`, a CIDR range list
    (`{ranges: []}` by default), from its connection establishment rate
    limiter, and counts them as `exemptedAdmissions`. This server does not
    register the parameter (measured 2026-11-12)."""
    answers = {}
    for server, client in (("rust", rust_mongo), ("mongod", mongod_rs)):
        try:
            reply = client.admin.command(
                "getParameter", 1, ingressConnectionEstablishmentRateLimiterBypass=1
            )
            answers[server] = reply["ingressConnectionEstablishmentRateLimiterBypass"]
        except pymongo.errors.OperationFailure as e:
            answers[server] = e.code
    assert answers["mongod"] == {"ranges": []}
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
    # A write that logged nothing reads the latest optime for the client's
    # last op under the global IS: twice for an insert, update or delete
    # (the write path and the command), once for another command.
    "insert of a duplicate key": lambda d: _w1(d).insert_one({"_id": 1}),
    "create of a collection there": lambda d: d.command("create", "c", writeConcern={"w": 1}),
    "drop of one not there": lambda d: d.command("drop", "k9", writeConcern={"w": 1}),
    "dropIndexes of one not there": lambda d: d.command(
        "dropIndexes", "c", index="nope_1", writeConcern={"w": 1}
    ),
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
    no flow control. A command other than `insert`, `update` and `delete`
    that took the global lock in IX and logged nothing (a `create` of a
    collection there, a `drop` or `dropIndexes` of one not there) reads the
    latest optime for the client's last op under the global IS, with the
    transition lock (`setLastOpToSystemLastOpTime`, measured 2026-11-12);
    this server did not."""
    answers = _write_locks(rust_mongo, mongod_rs, _MATCHING_WRITES[name])
    assert answers["mongod"]
    assert answers["rust"] == answers["mongod"]


#: Writes whose locks are not mongod's yet (RM-357's remainder).
_DIVERGING_WRITES = {
    # A delete that writes nothing reads the latest optime twice (the write
    # path and the command), and keeps its session row under its own locks.
    "delete matching nothing": lambda d: _w1(d).delete_one({"_id": 999}),
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


#: Aggregates on no collection whose pipelines read none.
_COLLECTIONLESS = {
    "$documents": [{"$documents": [{"a": 1}]}],
    "$documents, $match and $facet": [
        {"$documents": [{"a": 1}, {"a": 2}]},
        {"$match": {"a": 1}},
        {"$facet": {"x": [{"$project": {"a": 1}}]}},
    ],
    "$listLocalSessions": [{"$listLocalSessions": {}}],
}


@pytest.mark.backlog_fixed("RM-357")
@pytest.mark.parametrize("name", list(_COLLECTIONLESS))
def test_a_collectionless_aggregate_takes_no_lock(
    rust_mongo: Any, mongod_rs: Any, name: str
) -> None:
    """mongod 8.2.11 takes no lock for an `aggregate: 1` whose pipeline
    reads no collection (`runAggregate` on `<db>.$cmd.aggregate`): its line
    and entry print `locks: {}`, the line no execution admission and no
    `storage`, as the global lock was never taken, and the line names
    `<db>.$cmd.aggregate` (measured 2026-11-09). This server took the
    global IS, and an execution ticket with it, and named `<db>.$cmd`."""
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.aggregate(_COLLECTIONLESS[name])))
    out = {
        server: (
            [
                (
                    a.get("ns", "").split(".", 1)[-1],
                    a.get("locks"),
                    a.get("queues", {}).get("execution"),
                    "storage" in a,
                )
                for _, a in lines
            ],
            [(e.get("locks"), e.get("flowControl"), "storage" in e) for e in entries],
        )
        for server, (lines, entries) in got.items()
    }
    assert out["mongod"] == ([("$cmd.aggregate", {}, {}, False)], [({}, {}, False)])
    assert out["rust"] == out["mongod"]


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
    "a $jsonSchema predicate": {
        "find": "c",
        "filter": {"$jsonSchema": {"required": ["x"], "properties": {"x": {"bsonType": "int"}}}},
    },
}


@pytest.mark.backlog("RM-358")
def test_a_query_of_every_kind_names_its_query_shape(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2's `explain` (as its line and profile entry) carries a
    `queryShapeHash` for every `find`, `distinct` and `aggregate` it parses
    (measured 2026-11-07, and on 8.2.11 2026-11-09). This server shapes the
    predicates, operators, accumulators and stages in
    `tests/mongod_live/test_query_shape_hash.py`, `$rankFusion` and
    `$scoreFusion` among them since 2026-11-12, and prints none for
    `$jsonSchema`, which mongod shapes as the internal schema predicates it
    translates the schema into."""
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


def _contested(d: Any) -> None:
    seed(d)
    d.c.create_index("y")


def _contested_many(d: Any) -> None:
    d.c.insert_many([{"_id": n, "a": n % 7, "b": n % 11} for n in range(600)])
    d.c.create_index("a")
    d.c.create_index("b")


#: Reads two indexes could serve, which mongod plans by trial: the winner's
#: trial results are buffered when the command runs and returned first.
_CONTESTED: dict[str, tuple[Any, Any]] = {
    "ended_in_trial": (_contested, lambda d: list(d.c.find({"x": {"$gte": 2}, "y": 1}))),
    "ended_in_trial_batches": (
        _contested,
        lambda d: list(d.c.find({"x": {"$gte": 2}, "y": 1}).batch_size(1)),
    ),
    "buffered_101": (
        _contested_many,
        lambda d: list(d.c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).batch_size(40)),
    ),
    "buffered_limit": (
        _contested_many,
        lambda d: list(d.c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).limit(7).batch_size(3)),
    ),
    "buffered_skip": (
        _contested_many,
        lambda d: list(d.c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).skip(5).batch_size(60)),
    ),
    "buffered_sorted": (
        _contested_many,
        lambda d: list(
            d.c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).sort("_id", -1).batch_size(60)
        ),
    ),
    "buffered_no_first_batch": (
        _contested_many,
        lambda d: list(d.c.find({"a": {"$gte": 2}, "b": {"$gte": 3}}).batch_size(0)),
    ),
}


@pytest.mark.backlog_fixed("RM-349")
@pytest.mark.parametrize("name", list(_CONTESTED))
def test_a_contested_read_reports_its_winner_as_it_ran(
    rust_mongo: Any, steady_mongod: Any, name: str
) -> None:
    """mongod 8.2.11's profile entries for a `find` it planned by trial
    carry the winner's stages (no MULTI_PLAN), counted from the trial on:
    the `find`'s entry counts what the winner read in its trial and its
    first batch, each `getMore`'s what it read past the buffer (measured
    2026-11-09). This server read such a plan whole, through its store's
    router, and its entries carried none."""
    setup, op = _CONTESTED[name]
    got = _both(rust_mongo, steady_mongod, op, setup)
    answers = {
        s: [
            (e.get("keysExamined"), e.get("docsExamined"), _timeless(e.get("execStats")))
            for e in entries
        ]
        for s, (_, entries) in got.items()
    }
    assert answers["mongod"] and all(a[2] for a in answers["mongod"]), answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-349")
def test_a_rooted_or_of_tried_branches_counts_its_reads(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 tries each branch of a rooted `$or` on its own and drops
    the branches' trial results: the `find` and each `getMore` count what
    the composite plan read in that batch (measured 2026-11-09). Its stages
    are RM-459's."""
    query = {"$or": [{"a": {"$gte": 2}, "b": {"$gte": 3}}, {"a": 0, "b": 0}]}
    got = _both(
        rust_mongo,
        mongod_rs,
        lambda d: list(d.c.find(query).batch_size(60)),
        _contested_many,
    )
    answers = {
        s: [(e.get("keysExamined"), e.get("docsExamined"), e.get("nreturned")) for e in entries]
        for s, (_, entries) in got.items()
    }
    assert len(answers["mongod"]) == 6, answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-458")
def test_a_read_of_the_cached_plan_reports_its_cached_plan_stage(
    rust_mongo: Any, steady_mongod: Any
) -> None:
    """mongod 8.2.11's profile entries for a `find` that runs the active
    plan of its plan cache carry a CACHED_PLAN stage over the plan's, which
    counts what it handed out (`works` and `advanced` 2 on a first batch of
    two, 4 after the next, measured 2026-11-09). This server's started at
    the plan's FETCH; they now carry the CACHED_PLAN
    (`test_a_cached_read_reports_its_cached_plan_as_it_ran`)."""
    query = {"a": {"$gte": 2}, "b": {"$gte": 3}}

    def setup(d: Any) -> None:
        _contested_many(d)
        for _ in range(2):
            list(d.c.find(query))

    got = _both(
        rust_mongo,
        steady_mongod,
        lambda d: list(d.c.find(query).batch_size(40)),
        setup,
    )
    answers = {
        s: [_timeless(e.get("execStats")) for e in entries if e.get("op") in ("query", "getmore")]
        for s, (_, entries) in got.items()
    }
    assert [e["stage"] for e in answers["mongod"]] == ["CACHED_PLAN"] * 8, answers["mongod"]
    assert answers["rust"] == answers["mongod"]


_CACHED_QUERY = {"a": {"$gte": 2}, "b": {"$gte": 3}}

#: Reads of the plan cache's plan, besides `_CONTESTED`'s: a trial that
#: ran to its end before a default first batch, a LIMIT filled after the
#: trial in the first batch and in a `getMore`, a SKIP under a LIMIT, and a
#: first batch that reads the plan to its end.
_CACHED: dict[str, tuple[Any, Any]] = {
    **_CONTESTED,
    "ended_default_batch": (
        _contested,
        lambda d: list(d.c.find({"x": {"$gte": 2}, "y": 1})),
    ),
    "limit_past_trial": (
        _contested_many,
        lambda d: list(d.c.find(_CACHED_QUERY).limit(150).batch_size(200)),
    ),
    "limit_past_trial_batches": (
        _contested_many,
        lambda d: list(d.c.find(_CACHED_QUERY).limit(150).batch_size(120)),
    ),
    "skip_under_limit": (
        _contested_many,
        lambda d: list(d.c.find(_CACHED_QUERY).skip(3).limit(20).batch_size(8)),
    ),
    "whole_first_batch": (
        _contested_many,
        lambda d: list(d.c.find(_CACHED_QUERY).batch_size(500)),
    ),
}


@pytest.mark.backlog_fixed("RM-458")
@pytest.mark.parametrize("name", list(_CACHED))
def test_a_cached_read_reports_its_cached_plan_as_it_ran(
    rust_mongo: Any, steady_mongod: Any, name: str
) -> None:
    """mongod 8.2.11 runs a contested `find` whose plan cache entry is
    active under a CACHED_PLAN stage, which hands out its trial's results
    one per work before it works the plan again and passes on each work
    after; it finds a plan already at its end (a trial that ran out, a
    LIMIT filled) without working it, so the `find`'s last work is its own
    (measured 2026-11-10). This server's entries had no CACHED_PLAN."""
    setup, op = _CACHED[name]

    def cached(d: Any) -> None:
        setup(d)
        for _ in range(2):
            op(d)

    got = _both(rust_mongo, steady_mongod, op, cached)
    answers = {
        s: [
            (e.get("keysExamined"), e.get("docsExamined"), _timeless(e.get("execStats")))
            for e in entries
        ]
        for s, (_, entries) in got.items()
    }
    assert answers["mongod"], name
    assert all(a[2]["stage"] == "CACHED_PLAN" for a in answers["mongod"]), answers["mongod"]
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


@pytest.mark.backlog_fixed("RM-360")
def test_a_line_counts_its_interrupt_checks(rust_mongo: Any, steady_mongod: Any) -> None:
    """mongod 8.2.11's line for a command carries `numInterruptChecks`,
    how many times it checked whether it had been killed (5 for a `find`
    of ten documents through an index, measured 2026-10-27). This server's
    had none; it now checks where mongod does, and counts
    (`tests/mongod_live/test_interrupt_checks.py`).

    Each yield adds two checks (5, 9, 15, 27 for 0, 2, 5, 11 yields,
    measured 2026-11-10), and load decides whether a timed one comes: an RM
    gate saw mongod count 7. Both servers run with timed yields off."""
    with no_timed_yields(rust_mongo):
        got = _both(rust_mongo, steady_mongod, lambda d: list(d.c.find({"x": {"$gt": 1}})))
    answers = {s: _line(lines, "find").get("numInterruptChecks") for s, (lines, _) in got.items()}
    assert answers["mongod"] is not None
    assert answers["rust"] == answers["mongod"]


def _two_indexes(db: Any) -> None:
    seed(db)
    db.c.create_index("y")


_UNCOUNTED = {
    "find": (_two_indexes, lambda d: list(d.c.find({"x": {"$gt": 1}, "y": 1}))),
    "aggregate": (seed, lambda d: list(d.c.aggregate([{"$match": {"x": {"$gte": 4}}}]))),
    "insert": (
        seed,
        lambda d: d.c.insert_many([{"_id": 1}, {"_id": 100}], ordered=False),
    ),
    "findAndModify": (seed, lambda d: d.c.find_one_and_update({"_id": 1}, {"$set": {"q": 1}})),
    "createIndexes": (seed, lambda d: d.c.create_index("z")),
}


@pytest.mark.backlog("RM-360")
def test_every_line_counts_its_interrupt_checks(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for every command carries `numInterruptChecks`
    (measured 2026-11-09, time-based yields off: a `find` with two
    candidate plans 8, an `aggregate` 5, an unordered `insert` of two
    documents, one a duplicate key, 27, a retryable `findAndModify` 26, a
    `createIndexes` 73). This server counts its reads with one candidate
    plan and its `insert`, `update` and `delete`, and prints none here:
    mongod's planner checks once per index scan of every plan it
    enumerates, its pipeline stages every 128th document, a failed batch
    of inserts is inserted again a document at a time under locks of its
    own, a `findAndModify` takes rounds this server does not (RM-357), and
    DDL checks at points this server does not check yet."""
    for name, (setup, op) in _UNCOUNTED.items():
        answers = {}
        for server, client, is_mongod in (
            ("rust", rust_mongo, False),
            ("mongod", mongod_rs, True),
        ):
            with no_timed_yields(client):
                lines, _ = logged(client, is_mongod, setup, op)
            answers[server] = _line(lines, name).get("numInterruptChecks")
        assert answers["mongod"] is not None, name
        assert answers["rust"] == answers["mongod"], name


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


@pytest.mark.backlog_fixed("RM-379")
def test_an_async_oplog_write_says_how_long_it_held_its_oplog_slot(
    rust_mongo_async: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for an insert carries
    `totalOplogSlotDurationMicros` (RM-361). On the asynchronous oplog
    (`--oplog-async`) this server minted a write's entries only after the
    write committed, so it timed no slot and the line had no field; it now
    times each emit, where mongod reserves, to the write's commit."""
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


@pytest.mark.backlog_fixed("RM-363")
def test_a_contested_read_counts_what_it_examined(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `find` two indexes could serve counts what
    the plan it chose by trial examined. This server read such a plan
    through its store's router, and counted nothing, until RM-349 streamed
    the winner after its trial."""

    def setup(db: Any) -> None:
        seed(db)
        db.c.create_index("y")

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.find({"x": {"$gte": 2}, "y": 1})), setup)
    answers = {s: _counts(_line(lines, "find")) for s, (lines, _) in got.items()}
    assert "keysExamined" in answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-363")
def test_a_read_of_a_time_series_collection_counts_what_it_examined(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a `find` of a time-series collection counts
    the buckets it examined (`keysExamined: 0, docsExamined: 0` over none,
    measured 2026-10-27). This server read it whole, and counted nothing,
    until the `find` became the aggregation mongod converts it to (RM-455)."""

    def setup(db: Any) -> None:
        db.create_collection("ts", timeseries={"timeField": "t"})

    got = _both(rust_mongo, mongod_rs, lambda d: list(d.ts.find()), setup)
    answers = {s: _counts(_line(lines, "find")) for s, (lines, _) in got.items()}
    assert answers["mongod"].get("docsExamined") == 0
    assert answers["rust"] == answers["mongod"]


def _series(db: Any) -> None:
    """300 measurements in five series: five buckets."""
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    base = datetime.datetime(2024, 1, 1)
    db.ts.insert_many(
        [{"t": base + datetime.timedelta(seconds=i), "m": i % 5, "v": i} for i in range(300)]
    )


@pytest.mark.backlog_fixed("RM-363")
def test_a_time_series_find_counts_the_buckets_its_first_batch_examined(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a `find` of five buckets counts all five
    examined for the first batch, and none for the `getMore` after it, as
    its line for the same `aggregate` does (measured 2026-11-10)."""
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.ts.find({"v": {"$gte": 10}})), _series)
    answers = {s: [_counts(a) for _, a in lines] for s, (lines, _) in got.items()}
    assert [a.get("docsExamined") for a in answers["mongod"]] == [5, 0]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-363")
def test_a_time_series_read_made_whole_counts_what_it_examined(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a sorted `find` of five buckets counts the
    five it examined (measured 2026-11-10). This server reads a time-series
    collection whole under a sort, and counts nothing."""
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.ts.find().sort("v", -1).limit(3)), _series)
    answers = {s: _counts(_line(lines, "find")) for s, (lines, _) in got.items()}
    assert answers["mongod"].get("docsExamined") == 5
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-363")
def test_a_contested_aggregate_counts_what_it_examined(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for an `aggregate` two indexes could serve counts
    what the plan it chose by trial examined (`keysExamined: 4,
    docsExamined: 4`, measured 2026-11-10). This server reads it through its
    store's router, and counts nothing."""

    def setup(db: Any) -> None:
        seed(db)
        db.c.create_index("y")

    pipeline = [{"$match": {"x": {"$gte": 2}, "y": 1}}]
    got = _both(rust_mongo, mongod_rs, lambda d: list(d.c.aggregate(pipeline)), setup)
    answers = {s: _counts(_line(lines, "aggregate")) for s, (lines, _) in got.items()}
    assert "keysExamined" in answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-363")
def test_creating_a_view_inserts_its_definition(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 creates a view by inserting its definition into
    `system.views`, and its `create` line counts that key
    (`keysInserted: 1`, measured 2026-10-27). This server kept a view in
    its catalog and counted nothing; it now counts the `_id` key of the
    `system.views` document it presents for it."""
    got = _both(
        rust_mongo,
        mongod_rs,
        lambda d: d.command("create", "v", viewOn="c", pipeline=[]),
    )
    answers = {s: _line(lines, "create").get("keysInserted") for s, (lines, _) in got.items()}
    assert answers["mongod"] == 1
    assert answers["rust"] == answers["mongod"]


_KEYS = ("keysInserted", "keysDeleted")


def _with_view(db: Any) -> None:
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[])


def _with_ts(db: Any) -> None:
    db.create_collection("ts", timeseries={"timeField": "t"})


_DEFINITIONS = {
    "drop of a view": (_with_view, lambda d: d.drop_collection("v"), "drop"),
    "collMod of a view": (
        _with_view,
        lambda d: d.command("collMod", "v", viewOn="c", pipeline=[{"$match": {}}]),
        "collMod",
    ),
    "create of a time series": (
        lambda d: None,
        lambda d: d.command("create", "ts", timeseries={"timeField": "t", "metaField": "m"}),
        "create",
    ),
    "drop of a time series": (_with_ts, lambda d: d.drop_collection("ts"), "drop"),
    "create of an existing view": (
        _with_view,
        lambda d: d.command("create", "v", viewOn="c", pipeline=[]),
        "create",
    ),
    "create of a collection": (lambda d: None, lambda d: d.create_collection("p"), "create"),
    "drop of a collection": (seed, lambda d: d.drop_collection("c"), "drop"),
}


@pytest.mark.backlog_fixed("RM-363")
def test_a_definition_in_system_views_counts_its_key(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 keeps a view, and a time-series collection (a view over
    its buckets), as a `system.views` document: creating one inserts its
    `_id` key, dropping it deletes the key, and a `collMod` of a view
    updates it in place (`keysInserted: 0, keysDeleted: 0`). A plain
    collection's `create` and `drop`, and a `create` that finds the view
    there, count none (measured 2026-11-07)."""
    answers = {}
    for name, (setup, op, command) in _DEFINITIONS.items():
        got = _both(rust_mongo, mongod_rs, op, setup)
        answers[name] = {
            s: {k: _line(lines, command)[k] for k in _KEYS if k in _line(lines, command)}
            for s, (lines, _) in got.items()
        }
    assert answers["drop of a view"]["mongod"] == {"keysDeleted": 1}
    assert answers["collMod of a view"]["mongod"] == {"keysInserted": 0, "keysDeleted": 0}
    for name, a in answers.items():
        assert a["rust"] == a["mongod"], name


_PLANNED_APART = {
    "count": lambda d: d.command("count", "c"),
    "count with an empty query": lambda d: d.command("count", "c", query={}),
    "count of a missing collection": lambda d: d.command("count", "nope"),
    "find of a missing collection": lambda d: list(d.nope.find({"x": 1})),
}


@pytest.mark.backlog_fixed("RM-363")
def test_a_plan_settled_without_the_planner_is_timed(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 plans a `count` with no query (`RECORD_STORE_FAST_COUNT`)
    and a read of a collection that is not there (`EOF`) like any other, and
    their line and profile entry carry `planningTimeMicros` (measured
    2026-11-07). This server settled both without its planner and printed
    none."""
    answers = {}
    for name, op in _PLANNED_APART.items():
        got = _both(rust_mongo, mongod_rs, op)
        command = "find" if name.startswith("find") else "count"
        answers[name] = {
            s: (
                "planningTimeMicros" in _line(lines, command),
                ["planningTimeMicros" in e for e in entries],
            )
            for s, (lines, entries) in got.items()
        }
    for name, a in answers.items():
        assert a["mongod"] == (True, [True]), name
        assert a["rust"] == a["mongod"], name


# ------------------------------------------------------------- RM-364 --


@pytest.mark.backlog_fixed("RM-364")
def test_a_transaction_statement_counts_its_updates(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for each insert inside a transaction counts, in
    `storage.data`, the dirty bytes and the updates the statement added
    (`txnBytesDirty`, `txnNumUpdates`, measured 2026-10-27): 4 updates for
    a document over `_id` and one more index, 12 for three, each statement
    its own. The WiredTiger this server vendors (mongodb-7.0.33) kept the
    first and not the second, until `cmake/patch_wt_txn_updates.py` ported
    WiredTiger 11's count. (Only the `txn` keys are compared: whether the
    statement also read a page from disk is the cache's to say.)"""

    def op(db: Any) -> None:
        db.c.create_index("y")
        with db.client.start_session() as s, s.start_transaction():
            db.c.insert_one({"_id": 700, "y": 1}, session=s)
            db.c.insert_one({"_id": 701, "y": 2}, session=s)
            db.c.insert_many([{"_id": 800 + i} for i in range(3)], session=s)

    got = _both(rust_mongo, mongod_rs, op)
    answers = {}
    for s, (lines, _) in got.items():
        data = [
            attr.get("storage", {}).get("data", {})
            for _, attr in lines
            if next(iter(attr.get("command", {})), "") == "insert"
        ]
        answers[s] = [
            (sorted(k for k in d if k.startswith("txn")), d.get("txnNumUpdates")) for d in data
        ]
    keys = ["txnBytesDirty", "txnNumUpdates"]
    assert answers["mongod"] == [(keys, 4), (keys, 4), (keys, 12)]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-385")
def test_an_update_in_a_transaction_counts_no_dirty_bytes(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's `update` and `delete` command lines inside a
    transaction carried no `txnBytesDirty` or `txnNumUpdates`, where an
    insert's carries both (measured 2026-11-07): the statement's own line
    took them, as its `CurOp` takes its recovery unit's statistics since
    the last call. This server's printed the statement's share on the
    command line; its statement takes them now (2026-11-10)."""

    def op(db: Any) -> None:
        with db.client.start_session() as s, s.start_transaction():
            db.c.insert_one({"_id": 700, "y": 1}, session=s)
            db.c.update_one({"_id": 700}, {"$set": {"y": 5}}, session=s)
            db.c.delete_one({"_id": 700}, session=s)

    got = _both(rust_mongo, mongod_rs, op)
    answers = {
        s: {
            c: sorted(
                k for k in _line(lines, c).get("storage", {}).get("data", {}) if k.startswith("txn")
            )
            for c in ("insert", "update", "delete")
        }
        for s, (lines, _) in got.items()
    }
    assert answers["mongod"] == {
        "insert": ["txnBytesDirty", "txnNumUpdates"],
        "update": [],
        "delete": [],
    }
    assert answers["rust"] == answers["mongod"]
    # Where they went: the statements' lines and the profile entries.
    keys = ["txnBytesDirty", "txnNumUpdates"]
    taken = {
        s: (
            [
                (sorted(k for k in a.get("storage", {}).get("data", {}) if k.startswith("txn")))
                for _, a in lines
                if next(iter(a.get("command", {})), "") == "q"
            ],
            [
                (
                    e.get("op"),
                    sorted(k for k in e.get("storage", {}).get("data", {}) if k.startswith("txn")),
                )
                for e in entries
            ],
        )
        for s, (lines, entries) in got.items()
    }
    assert taken["mongod"] == (
        [keys, keys],
        [("insert", keys), ("update", keys), ("remove", keys)],
    )
    assert taken["rust"] == taken["mongod"]


# ------------------------------------------------------------- RM-499 --


@pytest.mark.backlog("RM-499")
def test_an_insert_in_a_transaction_counts_mongods_dirty_bytes(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a transaction's insert of `{_id: 700, y: 1}`
    into a collection indexed on `_id` and `x` counts about 305-345
    `txnBytesDirty`; this server's about 391, 71-87 bytes more per document
    (measured 2026-11-10), where an update and a delete of it count the same
    on both (96 and 192). An insert's count is not exact on either server:
    WiredTiger draws each new key's skiplist depth at random, 8 bytes a
    level (`__wt_skip_choose_depth`, one in four goes a level deeper), so
    the per-document mean over fifty documents is compared, within 8 bytes.
    The draw moves that mean by about 1.3 bytes either way; the surplus is
    ten times the tolerance."""
    n = 50

    def op(db: Any) -> None:
        with db.client.start_session() as s, s.start_transaction():
            db.c.insert_many([{"_id": 1000 + i, "y": i % 3} for i in range(n)], session=s)

    got = _both(rust_mongo, mongod_rs, op)
    per_doc = {
        s: [
            e.get("storage", {}).get("data", {}).get("txnBytesDirty", 0) / n
            for e in entries
            if e.get("op") == "insert"
        ]
        for s, (_, entries) in got.items()
    }
    assert len(per_doc["mongod"]) == 1 and len(per_doc["rust"]) == 1, per_doc
    assert abs(per_doc["rust"][0] - per_doc["mongod"][0]) <= 8, per_doc

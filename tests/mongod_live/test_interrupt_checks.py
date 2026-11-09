"""`numInterruptChecks` on a `Slow query` line, against a live mongod (RM-360).

mongod counts each time an operation checks whether it was killed or ran
out of time (`OperationContext::checkForInterruptNoAssert`), and the count
is its code structure: one at dispatch, one per lock it is granted, one per
index scan its planner builds and one when planning is done, the express
executor's, a getMore's once it has its cursor, one as a cursor left open
is checked back in, one for the lock it takes to read the storage
statistics for the line, and two per yield (the yield's, and the lock it
takes again). This server makes those checks where mongod makes them, and
counts them; each scenario compares the count, and the yields, line for
line. Both servers run with time-based yields off, so the counts are the
operation's own and not the machine's.

A write counts the checks of what follows it too: a retryable write's
session check-out, the reads of the system's latest optime a write that
logged nothing makes for its client's last op (its global IS lock and the
transition lock beside it, twice when the client's last op was already the
system's), and its write concern's waits -- the oplog's holes, the journal
flush for `j`, the commit point for `majority`, each a check before it
waits and one when it wakes.

What this server does not count yet prints no field, and is RM-360's
remainder: plans with several candidates, pipelines, `findAndModify`, DDL,
and the write concerns whose waits are not modelled (`fsync`, a numbered
`w` above 1, a tag, `majority` with `j: false`).
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest
from pymongo import WriteConcern

from tests.backlog.opdebug import logged, no_timed_yields, seed


def _seed_many(db: Any) -> None:
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(300)])
    db.c.create_index("x")


def _compound(db: Any) -> None:
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3} for n in range(12)])
    db.c.create_index([("x", 1), ("y", 1)])


#: label -> (setup, operation, the commands whose lines are compared).
SCENARIOS: dict[str, tuple[Callable[[Any], Any], Callable[[Any], Any], tuple[str, ...]]] = {
    "ping": (seed, lambda d: d.command("ping"), ("ping",)),
    "buildInfo": (seed, lambda d: d.command("buildInfo"), ("buildInfo",)),
    "find through an index": (seed, lambda d: list(d.c.find({"x": {"$gt": 1}})), ("find",)),
    "find through an index, nothing found": (
        seed,
        lambda d: list(d.c.find({"x": {"$gt": 100}})),
        ("find",),
    ),
    "find through a collection scan": (seed, lambda d: list(d.c.find({"z": 0})), ("find",)),
    "find of everything": (seed, lambda d: list(d.c.find()), ("find",)),
    "find by _id": (seed, lambda d: list(d.c.find({"_id": 3})), ("find",)),
    "find with a blocking sort": (
        seed,
        lambda d: list(d.c.find({"z": 0}).sort("y", 1)),
        ("find",),
    ),
    "find through an index, sorted": (
        seed,
        lambda d: list(d.c.find({"x": {"$gt": 1}}).sort("y", 1)),
        ("find",),
    ),
    "find with a projection": (
        seed,
        lambda d: list(d.c.find({"x": {"$gt": 1}}, {"x": 1, "_id": 0})),
        ("find",),
    ),
    "find with $in": (seed, lambda d: list(d.c.find({"x": {"$in": [1, 2, 3]}})), ("find",)),
    "find through a compound index": (
        _compound,
        lambda d: list(d.c.find({"x": {"$gt": 1}, "y": 1})),
        ("find",),
    ),
    "find of a collection not there": (seed, lambda d: list(d.nope.find({})), ("find",)),
    "find and its getMores": (
        _seed_many,
        lambda d: list(d.c.find({"x": {"$gte": 0}}, batch_size=100)),
        ("find", "getMore"),
    ),
    "find by a scan and its getMores": (
        _seed_many,
        lambda d: list(d.c.find({}, batch_size=100)),
        ("find", "getMore"),
    ),
    "count with no query": (seed, lambda d: d.command("count", "c"), ("count",)),
    "count through an index": (
        seed,
        lambda d: d.command("count", "c", query={"x": {"$gt": 3}}),
        ("count",),
    ),
    "count through a scan": (
        seed,
        lambda d: d.command("count", "c", query={"z": 0}),
        ("count",),
    ),
    "count by _id": (seed, lambda d: d.command("count", "c", query={"_id": 3}), ("count",)),
    "count of a collection not there": (
        seed,
        lambda d: d.command("count", "nope", query={"z": 0}),
        ("count",),
    ),
    "distinct through an index": (
        seed,
        lambda d: d.c.distinct("y", {"x": {"$lt": 7}}),
        ("distinct",),
    ),
    "distinct by a scan": (seed, lambda d: d.c.distinct("y"), ("distinct",)),
    "distinct of an indexed field": (seed, lambda d: d.c.distinct("x"), ("distinct",)),
    "listIndexes": (seed, lambda d: list(d.c.list_indexes()), ("listIndexes",)),
    "listCollections": (seed, lambda d: d.list_collection_names(), ("listCollections",)),
    "collStats": (seed, lambda d: d.command("collStats", "c"), ("collStats",)),
}


_W1 = WriteConcern(w=1)
_JOURNAL = WriteConcern(w=1, j=True)
_MAJORITY = WriteConcern(w="majority")


def _w1(db: Any) -> Any:
    return db.c.with_options(write_concern=_W1)


#: Writes on a member of a replica set: the driver's own (retryable, the
#: implicit `majority`), and the shapes each part of the count turns on.
WRITES: dict[str, tuple[Callable[[Any], Any], Callable[[Any], Any], tuple[str, ...]]] = {
    "insert": (seed, lambda d: d.c.insert_one({"_id": 100}), ("insert",)),
    "insert of several": (
        seed,
        lambda d: d.c.insert_many([{"_id": 100 + n} for n in range(5)]),
        ("insert",),
    ),
    "insert, w: 1": (seed, lambda d: _w1(d).insert_one({"_id": 100}), ("insert",)),
    "insert, j": (
        seed,
        lambda d: d.c.with_options(write_concern=_JOURNAL).insert_one({"_id": 100}),
        ("insert",),
    ),
    "insert of a duplicate key": (seed, lambda d: d.c.insert_one({"_id": 1}), ("insert",)),
    "insert of a duplicate key, w: 1": (
        seed,
        lambda d: _w1(d).insert_one({"_id": 1}),
        ("insert",),
    ),
    "insert not retryable": (
        seed,
        lambda d: d.command({"insert": "c", "documents": [{"_id": 100}]}),
        ("insert",),
    ),
    "insert not retryable, w: 1": (
        seed,
        lambda d: d.command({"insert": "c", "documents": [{"_id": 100}], "writeConcern": {"w": 1}}),
        ("insert",),
    ),
    "update": (seed, lambda d: d.c.update_one({"_id": 1}, {"$set": {"q": 1}}), ("update",)),
    "update, w: 1": (
        seed,
        lambda d: _w1(d).update_one({"_id": 1}, {"$set": {"q": 1}}),
        ("update",),
    ),
    "update, majority": (
        seed,
        lambda d: d.c.with_options(write_concern=_MAJORITY).update_one(
            {"_id": 1}, {"$set": {"q": 1}}
        ),
        ("update",),
    ),
    "update through an index": (
        seed,
        lambda d: d.c.update_one({"x": 1}, {"$set": {"q": 1}}),
        ("update",),
    ),
    "update matching nothing": (
        seed,
        lambda d: d.c.update_one({"_id": 99}, {"$set": {"q": 1}}),
        ("update",),
    ),
    "update matching nothing, w: 1": (
        seed,
        lambda d: _w1(d).update_one({"_id": 99}, {"$set": {"q": 1}}),
        ("update",),
    ),
    "update matching nothing, j": (
        seed,
        lambda d: d.c.with_options(write_concern=_JOURNAL).update_one(
            {"_id": 99}, {"$set": {"q": 1}}
        ),
        ("update",),
    ),
    "update changing nothing": (
        seed,
        lambda d: d.c.update_one({"_id": 1}, {"$set": {"x": 1}}),
        ("update",),
    ),
    "upsert": (
        seed,
        lambda d: d.c.update_one({"_id": 99}, {"$set": {"q": 1}}, upsert=True),
        ("update",),
    ),
    "update of many": (seed, lambda d: d.c.update_many({"z": 1}, {"$set": {"q": 1}}), ("update",)),
    "update of many, w: 1": (
        seed,
        lambda d: _w1(d).update_many({"z": 1}, {"$set": {"q": 1}}),
        ("update",),
    ),
    "update not retryable, matching nothing": (
        seed,
        lambda d: d.command(
            {"update": "c", "updates": [{"q": {"_id": 99}, "u": {"$set": {"q": 1}}}]}
        ),
        ("update",),
    ),
    "replace": (seed, lambda d: d.c.replace_one({"_id": 1}, {"a": 1}), ("update",)),
    "delete": (seed, lambda d: d.c.delete_one({"_id": 1}), ("delete",)),
    "delete, w: 1": (seed, lambda d: _w1(d).delete_one({"_id": 1}), ("delete",)),
    "delete matching nothing": (seed, lambda d: d.c.delete_one({"_id": 99}), ("delete",)),
    "delete of many": (seed, lambda d: d.c.delete_many({"z": 1}), ("delete",)),
    "delete of many, w: 1": (seed, lambda d: _w1(d).delete_many({"z": 1}), ("delete",)),
}


def _counts(
    client: Any, is_mongod: bool, scenario: tuple[Any, Any, tuple[str, ...]]
) -> list[tuple[str, Any, Any]]:
    setup, op, commands = scenario
    with no_timed_yields(client):
        lines, _ = logged(client, is_mongod, setup, op)
    out = []
    for _, attr in lines:
        name = next(iter(attr.get("command", {})), "")
        if name in commands:
            out.append((name, attr.get("numInterruptChecks"), attr.get("numYields")))
    return out


@pytest.mark.parametrize("label", list(SCENARIOS))
def test_a_line_counts_mongods_interrupt_checks(
    rust_mongo: Any, mongod_rs: Any, label: str
) -> None:
    scenario = SCENARIOS[label]
    theirs = _counts(mongod_rs, True, scenario)
    ours = _counts(rust_mongo, False, scenario)
    assert theirs, f"{label}: mongod wrote no line"
    assert all(n is not None for _, n, _ in theirs), theirs
    assert ours == theirs, f"{label}: ours {ours}, mongod {theirs}"


#: How many of mongod's runs a write's count is looked for in. Each wait a
#: write's write concern makes -- the oplog's holes, the `j` journal flush,
#: the `majority` commit point -- is a race inside mongod. Each waits on a
#: future (`JournalFlusher::waitForJournalFlush`, `awaitReplication`'s
#: `_futureGetNoThrowWithDeadline`), and a future already finished returns
#: without a check: `_startWaitingForReplication` hands back a ready one
#: when the commit point is there already, and `SharedStateBase::wait`
#: (8.2.12 `future_impl.h`) returns early on a finished state. A future
#: that finishes while it is being waited on skips only the wake's check.
#: So mongod counts the wait's two checks, one or none, by which thread won.
#: macOS mongod 8.2.11, whose journal sync takes 4-8 ms, waited every time
#: (25 runs of each write, 2026-11-09). Linux mongod 8.2.12 in CI did not,
#: twice in a day: 17 against 19 for `update matching nothing, j`, 18
#: against 19 for `insert of several`. This server flushes and advances
#: its commit point on the request thread, so it always counts the branch
#: that waited, which is one of mongod's answers.
MONGOD_RUNS = 5


@pytest.mark.parametrize("label", list(WRITES))
def test_a_write_line_counts_mongods_interrupt_checks(
    rust_mongo: Any, mongod_rs: Any, label: str
) -> None:
    scenario = WRITES[label]
    theirs = _counts(mongod_rs, True, scenario)
    ours = _counts(rust_mongo, False, scenario)
    assert theirs, f"{label}: mongod wrote no line"
    assert all(n is not None for _, n, _ in theirs), theirs
    seen = [theirs]
    while ours != seen[-1] and len(seen) < MONGOD_RUNS:
        seen.append(_counts(mongod_rs, True, scenario))
    assert ours in seen, f"{label}: ours {ours}, mongod's runs {seen}"


def test_the_yield_period_is_a_parameter(rust_mongo: Any) -> None:
    """mongod's `internalQueryExecYieldPeriodMS` (default 10, at least 0)
    sets how long a plan executor runs between yields; this server's
    executors keep it."""
    admin = rust_mongo.admin
    assert (
        admin.command("getParameter", 1, internalQueryExecYieldPeriodMS=1)[
            "internalQueryExecYieldPeriodMS"
        ]
        == 10
    )
    was = admin.command("setParameter", 1, internalQueryExecYieldPeriodMS=25)["was"]
    try:
        assert was == 10
        got = admin.command("getParameter", 1, internalQueryExecYieldPeriodMS=1)
        assert got["internalQueryExecYieldPeriodMS"] == 25
    finally:
        admin.command("setParameter", 1, internalQueryExecYieldPeriodMS=was)

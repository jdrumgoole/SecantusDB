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

A walk of a whole index -- the scan a sort is read in, or a hinted index
with no predicate on its leading field -- is built outside mongod's access
planner (`QueryPlannerAccess::scanWholeIndex`) and makes no check.

An `aggregate` that mongod's query layer runs whole checks as a `find` does.
One that reads through its cursor stage checks once for each buffer that
stage fills -- the global IS it takes for it -- and its stages tick the
pipeline's interrupt checker once per `getNext`, checking every 128th
tick; the checker lives with the cursor, so a `getMore` goes on counting
where the batch before stopped. A `$limit` stops asking its source once it has
passed its count, and a cursor stage over a plan with a limit fills one
buffer to it, with no first buffer of 32. Such a `getMore` takes no lock of its own,
and so reads no storage statistics unless it filled a buffer.

A `$sort` after a `$group` the slot-based engine runs reads the groups
through the cursor stage in the same way: a global IS per buffer, and a
tick for each document the `$sort` pulls from it, each it passes on, and
each end found; so does a `$sort` after stages that pass one document on
for each they take (`$addFields`, `$set`, `$unset`, `$project`,
`$replaceRoot`, `$replaceWith`), each of them pulled once per document and
once more for the end.

What this server does not count yet prints no field, and is RM-360's
remainder: plans with several candidates, the pipelines it does not model,
`findAndModify`, DDL, and the write concerns whose waits are not modelled
(`fsync`, a numbered `w` above 1, a tag, `majority` with `j: false`).
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


def _documents(n: int) -> Callable[[Any], Any]:
    """`n` documents, each with an array of none to three elements, over an
    index on `x`: enough to fill several cursor-stage buffers (32, then
    twice as many each time) and to cross the pipeline checker's 128 ticks
    inside and between batches."""

    def setup(db: Any) -> None:
        db.c.insert_many(
            [{"_id": n, "x": n, "y": n % 3, "z": n % 2, "a": list(range(n % 4))} for n in range(n)]
        )
        db.c.create_index("x")

    return setup


def _arrays(n: int) -> Callable[[Any], Any]:
    """Twelve documents, each with an array `a` of `n` elements."""
    return lambda db: db.c.insert_many([{"_id": i, "a": list(range(n))} for i in range(12)])


def _pipeline(pipeline: list, **kwargs: Any) -> Callable[[Any], Any]:
    return lambda d: list(d.c.aggregate(pipeline, **kwargs))


_ADD = {"$addFields": {"q": 1}}

_GROUP_SORT = [{"$group": {"_id": "$y"}}, {"$sort": {"_id": 1}}]


def _groups(n: int) -> Callable[[Any], Any]:
    """`n` documents over an index on `x`, each its own group of `y`."""

    def setup(db: Any) -> None:
        db.c.insert_many([{"_id": i, "x": i, "y": i} for i in range(n)])
        db.c.create_index("x")

    return setup


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
    "find sorted by a walk of a whole index": (
        seed,
        lambda d: list(d.c.find({}).sort("x", -1)),
        ("find",),
    ),
    "find filtered, sorted by a walk of a whole index": (
        seed,
        lambda d: list(d.c.find({"y": 1}).sort("x", 1)),
        ("find",),
    ),
    "find through a hinted index it does not bound": (
        seed,
        lambda d: list(d.c.find({"y": 1}).hint([("x", 1)])),
        ("find",),
    ),
    "count through a hinted index": (
        seed,
        lambda d: d.command("count", "c", query={}, hint={"x": 1}),
        ("count",),
    ),
}


_PIPELINE = ("aggregate", "getMore")

#: Aggregates of a collection: the ones mongod's query layer runs whole, and
#: the ones through its cursor stage whose stages are counted here.
PIPELINES: dict[str, tuple[Callable[[Any], Any], Callable[[Any], Any], tuple[str, ...]]] = {
    "aggregate of nothing but a $match": (
        seed,
        _pipeline([{"$match": {"x": {"$gte": 4}}}]),
        _PIPELINE,
    ),
    "aggregate of no stages": (seed, _pipeline([]), _PIPELINE),
    "aggregate of a $project": (seed, _pipeline([{"$project": {"x": 1}}]), _PIPELINE),
    "aggregate sorted": (seed, _pipeline([{"$sort": {"y": 1}}]), _PIPELINE),
    "aggregate sorted by an index": (_documents(300), _pipeline([{"$sort": {"x": 1}}]), _PIPELINE),
    "aggregate of a $limit": (seed, _pipeline([{"$limit": 4}]), _PIPELINE),
    "aggregate of a $group": (
        seed,
        _pipeline([{"$match": {"x": {"$gt": 2}}}, {"$group": {"_id": "$y", "n": {"$sum": 1}}}]),
        _PIPELINE,
    ),
    "aggregate of a collection not there": (
        seed,
        lambda d: list(d.nope.aggregate([{"$match": {"x": 1}}])),
        _PIPELINE,
    ),
    "aggregate through its cursor stage": (seed, _pipeline([_ADD]), _PIPELINE),
    "aggregate of one buffer, full": (_documents(32), _pipeline([_ADD]), _PIPELINE),
    "aggregate of a first batch, full": (_documents(101), _pipeline([_ADD]), _PIPELINE),
    "aggregate and its getMores": (_documents(1000), _pipeline([_ADD]), _PIPELINE),
    "aggregate in small batches": (_documents(300), _pipeline([_ADD], batchSize=50), _PIPELINE),
    # A first batch of none plans the pipeline's front and reads nothing:
    # 5 on mongod 8.2.11, 6 with an index scan, as a `find` of no batch
    # (measured 2026-11-18); its `getMore` reads it all.
    "aggregate of a first batch of none": (seed, _pipeline([], batchSize=0), _PIPELINE),
    "aggregate of no batch through its cursor stage": (
        seed,
        _pipeline([_ADD], batchSize=0),
        _PIPELINE,
    ),
    "aggregate of no batch over an index": (
        seed,
        _pipeline([{"$match": {"x": {"$gte": 4}}}], batchSize=0),
        _PIPELINE,
    ),
    "aggregate of no batch sorting": (
        seed,
        _pipeline([{"$sort": {"y": 1}}], batchSize=0),
        _PIPELINE,
    ),
    "aggregate of several stages": (
        _documents(300),
        _pipeline(
            [
                {"$match": {"x": {"$gte": 3}}},
                {"$project": {"x": 1, "a": 1}},
                {"$set": {"w": {"$add": ["$x", 1]}}},
                {"$unset": "a"},
                {"$replaceRoot": {"newRoot": {"v": "$w"}}},
            ]
        ),
        _PIPELINE,
    ),
    "aggregate with an $unwind": (_documents(400), _pipeline([{"$unwind": "$a"}]), _PIPELINE),
    "aggregate with an $unwind, batches ending inside a document": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$set": {"b": 1}}], batchSize=33),
        _PIPELINE,
    ),
    "aggregate with a $skip": (
        _documents(400),
        _pipeline([_ADD, {"$skip": 150}], batchSize=40),
        _PIPELINE,
    ),
    "aggregate with a $limit after an $unwind": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$limit": 50}]),
        _PIPELINE,
    ),
    "aggregate with a $limit after an $unwind, over several batches": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$limit": 300}, {"$set": {"b": 1}}], batchSize=40),
        _PIPELINE,
    ),
    "aggregate with a $limit past the end": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$limit": 5000}]),
        _PIPELINE,
    ),
    "aggregate with a $skip and a $limit after an $unwind": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$skip": 5}, {"$limit": 20}]),
        _PIPELINE,
    ),
    "aggregate with a $limit after an $addFields": (
        _documents(400),
        _pipeline([_ADD, {"$limit": 50}]),
        _PIPELINE,
    ),
    "aggregate with a $skip between two $unwinds": (
        _documents(400),
        _pipeline([{"$unwind": "$a"}, {"$skip": 5}, {"$unwind": "$a"}], batchSize=60),
        _PIPELINE,
    ),
    # mongod 8.2.11 checks after every tenth element of each evaluation of a
    # `$map`, `$filter` or `$reduce`, its count starting again with the
    # next (RM-360): 9 elements check none, 10 once, 50 five times.
    "aggregate with a $map of 2": (
        seed,
        _pipeline([{"$addFields": {"m": {"$map": {"input": [1, 2], "in": "$$this"}}}}]),
        _PIPELINE,
    ),
    "aggregate with a $map of 9": (
        _arrays(9),
        _pipeline([{"$addFields": {"m": {"$map": {"input": "$a", "in": "$$this"}}}}]),
        _PIPELINE,
    ),
    "aggregate with a $map of 10": (
        _arrays(10),
        _pipeline([{"$addFields": {"m": {"$map": {"input": "$a", "in": "$$this"}}}}]),
        _PIPELINE,
    ),
    "aggregate with a $map of 127": (
        _arrays(127),
        _pipeline([{"$addFields": {"m": {"$map": {"input": "$a", "in": "$$this"}}}}]),
        _PIPELINE,
    ),
    "aggregate with two $maps of 5 and a nested one": (
        _arrays(5),
        _pipeline(
            [
                {
                    "$addFields": {
                        "m": {"$map": {"input": "$a", "in": "$$this"}},
                        "n": {"$map": {"input": "$a", "in": {"$map": {"input": "$a", "in": 1}}}},
                    }
                }
            ]
        ),
        _PIPELINE,
    ),
    "aggregate with a $filter of 50": (
        _arrays(50),
        _pipeline([{"$addFields": {"m": {"$filter": {"input": "$a", "cond": True}}}}]),
        _PIPELINE,
    ),
    "aggregate with a $filter stopped by its limit at 10": (
        _arrays(50),
        _pipeline([{"$addFields": {"m": {"$filter": {"input": "$a", "cond": True, "limit": 10}}}}]),
        _PIPELINE,
    ),
    # A `$sort` after a `$group` mongod's slot-based engine runs reads the
    # groups through the cursor stage: a global IS per buffer it loads, and
    # a tick per `getNext` of the `$sort` and of the cursor stage beneath it
    # (RM-360, measured 8.2.11 2026-11-17: 32 groups 6, 96 groups 8, 300
    # groups 12 and a `getMore` of 3).
    "aggregate sorting three groups": (seed, _pipeline(_GROUP_SORT), _PIPELINE),
    "aggregate sorting 32 groups, one buffer full": (
        _groups(32),
        _pipeline(_GROUP_SORT),
        _PIPELINE,
    ),
    "aggregate sorting 96 groups": (_groups(96), _pipeline(_GROUP_SORT), _PIPELINE),
    "aggregate sorting 101 groups, a first batch full": (
        _groups(101),
        _pipeline(_GROUP_SORT),
        _PIPELINE,
    ),
    "aggregate sorting 400 groups, and its getMore": (
        _groups(400),
        _pipeline(_GROUP_SORT),
        _PIPELINE,
    ),
    "aggregate sorting 300 groups in small batches": (
        _groups(300),
        _pipeline(_GROUP_SORT, batchSize=50),
        _PIPELINE,
    ),
    "aggregate sorting groups, a first batch of none": (
        seed,
        _pipeline(_GROUP_SORT, batchSize=0),
        _PIPELINE,
    ),
    "aggregate sorting 300 groups, a first batch of none": (
        _groups(300),
        _pipeline(_GROUP_SORT, batchSize=0),
        _PIPELINE,
    ),
    "aggregate sorting the groups of a $match": (
        seed,
        _pipeline([{"$match": {"x": {"$gt": 2}}}, *_GROUP_SORT]),
        _PIPELINE,
    ),
    # A `$count`, `$sortByCount` or `$bucket` is a `$group` SBE takes and a
    # `$project` or `$sort` the pipeline runs over the cursor stage, counted
    # as a `$sort` after a `$group` is (RM-360, measured 8.2.11 2026-11-17:
    # a `$count` 5 at any size; a `$sortByCount` of 300 groups 12 and a
    # `getMore` of 3).
    # A `$sort` after stages that pass one document on for each, over a
    # classic read: each is pulled once per document and once for the end,
    # as the cursor stage is (measured 8.2.11, 2026-11-18).
    "aggregate sorting after an $addFields": (
        _documents(300),
        _pipeline([_ADD, {"$sort": {"q": 1, "_id": 1}}]),
        _PIPELINE,
    ),
    "aggregate sorting by an index after an $addFields": (
        _documents(300),
        _pipeline([_ADD, {"$sort": {"x": 1}}]),
        _PIPELINE,
    ),
    "aggregate sorting after a $project the plan takes": (
        _documents(300),
        _pipeline([{"$project": {"y": 1}}, {"$sort": {"y": 1, "_id": 1}}]),
        _PIPELINE,
    ),
    "aggregate sorting after $match, $set and $unset, batches of 40": (
        _documents(400),
        _pipeline(
            [
                {"$match": {"x": {"$gte": 3}}},
                {"$set": {"w": 1}},
                {"$unset": "a"},
                {"$sort": {"y": -1, "_id": 1}},
            ],
            batchSize=40,
        ),
        _PIPELINE,
    ),
    "aggregate sorting after a $replaceRoot, one buffer": (
        _documents(20),
        _pipeline(
            [{"$replaceRoot": {"newRoot": {"v": "$y", "i": "$_id"}}}, {"$sort": {"v": 1, "i": 1}}]
        ),
        _PIPELINE,
    ),
    "aggregate sorting after an $addFields, a first batch of none": (
        _documents(300),
        _pipeline([_ADD, {"$sort": {"q": 1, "_id": 1}}], batchSize=0),
        _PIPELINE,
    ),
    "aggregate counting": (_groups(300), _pipeline([{"$count": "n"}]), _PIPELINE),
    "aggregate counting groups": (
        seed,
        _pipeline([{"$group": {"_id": "$y"}}, {"$count": "n"}]),
        _PIPELINE,
    ),
    "aggregate with a $sortByCount of 300": (
        _groups(300),
        _pipeline([{"$sortByCount": "$y"}]),
        _PIPELINE,
    ),
    "aggregate with a $sortByCount of groups": (
        _groups(300),
        _pipeline([{"$group": {"_id": "$y"}}, {"$sortByCount": "$_id"}]),
        _PIPELINE,
    ),
    "aggregate with a $bucket": (
        _groups(300),
        _pipeline([{"$bucket": {"groupBy": "$y", "boundaries": [0, 1000]}}]),
        _PIPELINE,
    ),
    # 49 buckets, the most SBE takes: a `$bucket` of 50 is a `$switch` of 50
    # branches, which keeps its `$group` classic.
    "aggregate with a $bucket of 49 buckets": (
        _groups(300),
        _pipeline([{"$bucket": {"groupBy": "$y", "boundaries": [*range(0, 294, 6), 1000]}}]),
        _PIPELINE,
    ),
    "aggregate with a $reduce of 50": (
        _arrays(50),
        _pipeline(
            [
                {
                    "$addFields": {
                        "m": {"$reduce": {"input": "$a", "initialValue": 0, "in": "$$value"}}
                    }
                }
            ]
        ),
        _PIPELINE,
    ),
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


@pytest.mark.parametrize("label", list(PIPELINES))
def test_a_pipeline_line_counts_mongods_interrupt_checks(
    rust_mongo: Any, mongod_rs: Any, label: str
) -> None:
    """Every line of the aggregate and its getMores, and their `locks`: a
    cursor stage's buffer takes the global IS each time it fills, and a
    getMore that fills none takes no lock (measured 8.2.11, 2026-11-12)."""
    scenario = PIPELINES[label]
    setup, op, commands = scenario
    answers = {}
    for name, client, is_mongod in (("mongod", mongod_rs, True), ("rust", rust_mongo, False)):
        with no_timed_yields(client):
            lines, _ = logged(client, is_mongod, setup, op)
        answers[name] = [
            (
                next(iter(attr["command"])),
                attr.get("numInterruptChecks"),
                attr.get("numYields"),
                attr.get("locks", {}).get("Global"),
            )
            for _, attr in lines
            if next(iter(attr.get("command", {})), "") in commands
        ]
    theirs, ours = answers["mongod"], answers["rust"]
    assert theirs, f"{label}: mongod wrote no line"
    assert all(n is not None for _, n, _, _ in theirs), theirs
    assert ours == theirs, f"{label}: ours {ours}, mongod {theirs}"


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

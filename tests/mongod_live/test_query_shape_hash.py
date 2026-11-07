"""`queryShapeHash` against mongod 8.2 (RM-358): the SHA-256 of a `find`'s,
`distinct`'s or `aggregate`'s representative query shape, which mongod
prints on its `Slow query` line, its profile entry and its `explain`.

Each case runs `explain` of the same command on both servers, in a database
and collection of the same names (the hash covers the namespace), and
compares the hashes. The cases sweep what the shape is made of: the filter
as mongod parses it (not as it optimizes it), the literal each value stands
for, the projection, the sort and the options, and the pipeline stages a
command is parsed into.
"""

from __future__ import annotations

import datetime
from typing import Any

import pytest
from bson import Binary, Decimal128, Int64, ObjectId, Regex
from bson.son import SON

_DB = "qshape_live"

FILTERS: list[Any] = [
    {},
    {"x": 1},
    SON([("y", 5), ("x", 2)]),
    {"x": {"$gt": 1, "$lt": 5}},
    {"x": {"$in": [3, "a", 1]}},
    {"x": {"$in": [1, Int64(2)]}},
    {"x": {"$in": [1.5, 2]}},
    {"x": {"$in": []}},
    {"x": {"$in": [5]}},
    {"x": {"$in": [Regex("a")]}},
    {"x": {"$in": [1, Regex("a")]}},
    {"x": {"$in": [[1], [2]]}},
    {"x": {"$in": [None]}},
    {"x": {"$eq": [1, Int64(2)]}},
    {"x": {"$eq": Regex("a")}},
    {"x": Regex("a")},
    {"x": Regex("a", "i")},
    {"x": {"$regex": "a", "$options": "si"}},
    {"x": {"$options": "i", "$regex": "a"}},
    {"x": {"$regex": Regex("a", "i")}},
    {"x": {"$not": {"$gt": 1, "$lt": 5}}},
    {"x": {"$not": {"$gt": 1}}},
    {"x": {"$not": Regex("a")}},
    {"$or": [{"x": 1}, {"x": 2}]},
    {"$or": [{"x": 1}, {"y": 2}]},
    {"$and": [{"x": 1}]},
    {"$nor": [{"x": 1}, {"y": {"$lt": 3}}]},
    {"$or": [{"x": 1, "y": 2}, {"z": 3}]},
    {"x": {"$elemMatch": {"$gt": 1, "$lt": 5}}},
    {"x": {"$elemMatch": {"a": 1, "b": 2}}},
    {"x": {"$elemMatch": {"a": 1}}},
    {"x": {"$elemMatch": {"$not": {"$gt": 1}}}},
    {"x": {"$elemMatch": {"$and": [{"a": 1}]}}},
    {"x": {"$all": [1, 2]}},
    {"x": {"$all": [1]}},
    {"x": {"$all": []}},
    {"x": {"$all": [{"$elemMatch": {"a": 1}}]}},
    {"x": {"$type": "number"}},
    {"x": {"$type": ["string", 1, "number"]}},
    {"x": {"$type": [16, 2]}},
    {"x": {"$bitsAllSet": 5}},
    {"x": {"$bitsAnySet": [1, 3]}},
    {"x": {"$bitsAllClear": 0}},
    {"x": {"$bitsAnyClear": Binary(b"\x01")}},
    {"x": {"$exists": True}},
    {"x": {"$exists": 0}},
    {"x": None},
    {"x": {"$ne": None}},
    {"x": {"$ne": 3}},
    {"x": {"$nin": [3, 4]}},
    {"x": datetime.datetime(2020, 1, 1)},
    {"x": ObjectId("65a000000000000000000000")},
    {"x": Decimal128("1.5")},
    {"x": True},
    {"x": "s"},
    {"x": {"a": 1, "b": 2}},
    {"x": [1, 2]},
    {"x": [1, "a"]},
    {"x": [[1, 2]]},
    {"x": []},
    {"x.y": {"$size": 2}},
    {"$comment": "hi", "x": 1},
    {"$alwaysTrue": 1},
    {"$alwaysFalse": 1},
    {"x": {"$mod": [3, 0]}},
    {"x": {"$not": {"$elemMatch": {"a": 1}}}},
    {"x": {"$not": {"$exists": True}}},
    {"x": {"$gte": Int64(5)}},
    {"$expr": {"$gt": ["$x", 2]}},
    {"$expr": {"$eq": ["$x", {"$add": ["$y", 1]}]}},
]

FIND_OPTIONS: list[dict] = [
    {"projection": {"x": 1}},
    {"projection": {"x": 0}},
    {"projection": SON([("x", 1), ("_id", 0)])},
    {"projection": SON([("_id", 1), ("x", 1)])},
    {"projection": {"a.b": 1, "a.c": 1}},
    {"projection": {"y": "$x"}},
    {"projection": {"y": "lit"}},
    {"projection": {"y": {"$add": ["$x", 1]}}},
    {"projection": {"y": {"$slice": 3}}},
    {"projection": {"y": {"$slice": [1, 3]}}},
    {"projection": {"y": {"$elemMatch": {"a": 1}}}},
    {"projection": {}},
    {"sort": {"x": -1}},
    {"sort": SON([("x", 1), ("y", -1)])},
    {"sort": {"x": 1.0}},
    {"limit": 3},
    {"skip": 3},
    {"limit": 3, "skip": 2},
    {"singleBatch": True},
    {"singleBatch": False},
    {"allowDiskUse": True},
    {"returnKey": True},
    {"showRecordId": False},
    {"batchSize": 2},
    {"collation": {"locale": "fr"}},
    {"let": {"v": 5, "w": {"$add": [1, 2]}, "s": {"$literal": "a"}}},
    {"hint": {"x": 1}, "min": {"x": 2}, "max": {"x": 8}},
]

PIPELINES: list[list] = [
    [],
    [{"$match": {"x": {"$gt": 1}}}, {"$sort": {"x": -1}}, {"$limit": 3}, {"$skip": 1}],
    [{"$count": "n"}],
    [
        {
            "$group": {
                "_id": "$y",
                "n": {"$sum": 1},
                "t": {"$sum": "$x"},
                "m": {"$max": "$x"},
                "lo": {"$min": "$x"},
                "p": {"$push": "$x"},
                "f": {"$first": "$x"},
                "l": {"$last": "$x"},
            }
        }
    ],
    [{"$group": {"_id": 1, "n": {"$sum": 1}}}],
    [{"$group": {"_id": {"a": "$y", "b": "$x"}, "c": {"$count": {}}}}],
    [{"$group": {"_id": None, "c": {"$avg": "$x"}, "s": {"$addToSet": "$y"}}}],
    [{"$match": {"x": 1}}, {"$group": {"_id": 1, "n": {"$sum": 1}}}],
    [{"$project": {"x": 1, "z": {"$add": ["$x", 1]}, "w": "lit", "q": {"$literal": 5}}}],
    [{"$project": {"x": 0}}],
    [{"$project": SON([("x", 1), ("_id", 0)])}],
    [{"$project": {"a.b": 1, "x": "$y"}}],
    [
        {
            "$project": {
                "c": {"$cond": [{"$gt": ["$x", 1]}, "a", "b"]},
                "s": {"$size": "$a"},
                "e": {"$eq": ["$x", 1]},
            }
        }
    ],
    [{"$addFields": {"z": {"$multiply": ["$x", 2]}, "o": {"k": 1}, "arr": [1, 2]}}],
    [{"$addFields": {"k": {"$concat": ["a", "$y"]}, "l": {"$literal": [1]}}}],
    [{"$set": {"z": "$x"}}],
    [{"$unset": "x"}],
    [{"$unset": ["x", "y"]}],
    [{"$unwind": "$a"}],
    [{"$unwind": {"path": "$a", "includeArrayIndex": "i", "preserveNullAndEmptyArrays": True}}],
    [{"$unwind": {"path": "$a", "preserveNullAndEmptyArrays": False}}],
    [{"$replaceRoot": {"newRoot": {"v": "$x"}}}],
    [{"$replaceWith": "$$ROOT"}],
    [{"$lookup": {"from": "o", "localField": "x", "foreignField": "k", "as": "j"}}],
    [{"$sample": {"size": 2}}],
    [{"$sortByCount": "$y"}],
    [{"$match": {"$expr": {"$gt": ["$x", 2]}}}],
]

DISTINCTS: list[tuple[str, Any]] = [
    ("x", None),
    ("x", {"x": {"$gt": 5}}),
    ("y.z", SON([("y", 5), ("x", {"$in": [1, "a"]})])),
]


@pytest.fixture(scope="module")
def servers(rust_mongo: Any, mongod_rs: Any) -> Any:
    for client in (rust_mongo, mongod_rs):
        db = client[_DB]
        db.c.drop()
        db.o.drop()
        db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "a": [1, 2]} for n in range(6)])
        db.c.create_index("x")
        db.o.insert_one({"k": 1})
    yield rust_mongo, mongod_rs
    for client in (rust_mongo, mongod_rs):
        client.drop_database(_DB)


def _hashes(servers: Any, command: SON) -> tuple[Any, Any]:
    out = []
    for client in servers:
        reply = client[_DB].command("explain", command, verbosity="queryPlanner")
        out.append(reply.get("queryShapeHash"))
    return out[0], out[1]


def _check(servers: Any, command: SON) -> None:
    ours, theirs = _hashes(servers, command)
    assert theirs, f"mongod printed no queryShapeHash for {command}"
    assert ours == theirs, command


@pytest.mark.parametrize("filt", FILTERS, ids=[str(f) for f in FILTERS])
def test_a_finds_filter(servers: Any, filt: Any) -> None:
    _check(servers, SON([("find", "c"), ("filter", filt)]))


@pytest.mark.parametrize("options", FIND_OPTIONS, ids=[str(o) for o in FIND_OPTIONS])
def test_a_finds_options(servers: Any, options: dict) -> None:
    command = SON([("find", "c"), ("filter", {"x": {"$gt": 1}})])
    command.update(options)
    _check(servers, command)


@pytest.mark.parametrize("pipeline", PIPELINES, ids=[str(p) for p in PIPELINES])
def test_an_aggregates_pipeline(servers: Any, pipeline: list) -> None:
    _check(servers, SON([("aggregate", "c"), ("pipeline", pipeline), ("cursor", {})]))


def test_an_aggregates_options(servers: Any) -> None:
    _check(
        servers,
        SON(
            [
                ("aggregate", "c"),
                ("pipeline", [{"$match": {"x": 1}}]),
                ("cursor", {}),
                ("collation", {"locale": "fr"}),
                ("let", {"v": 1}),
                ("allowDiskUse", True),
            ]
        ),
    )


@pytest.mark.parametrize("key,query", DISTINCTS, ids=[str(d) for d in DISTINCTS])
def test_a_distinct(servers: Any, key: str, query: Any) -> None:
    command = SON([("distinct", "c"), ("key", key)])
    if query is not None:
        command["query"] = query
    _check(servers, command)


def test_the_hash_follows_the_plan_in_explain(servers: Any) -> None:
    """mongod puts `queryShapeHash` right after the plan: `executionStats`,
    else `queryPlanner`, else a pipeline's `stages`. (Which of these an
    `aggregate` has differs where mongod runs the pipeline in SBE, an
    explain difference of its own in the backlog.)"""
    commands = [
        SON([("find", "c"), ("filter", {"x": 1})]),
        SON([("distinct", "c"), ("key", "x")]),
        SON([("aggregate", "c"), ("pipeline", [{"$group": {"_id": "$y"}}]), ("cursor", {})]),
        SON([("aggregate", "c"), ("pipeline", [{"$unwind": "$a"}]), ("cursor", {})]),
    ]
    for command in commands:
        for verbosity in ("queryPlanner", "executionStats"):
            for client in servers:
                keys = list(client[_DB].command("explain", command, verbosity=verbosity))
                plan = [k for k in ("stages", "queryPlanner", "executionStats") if k in keys]
                at = keys.index("queryShapeHash")
                assert keys[at - 1] == plan[-1], (command, verbosity, keys)


def test_a_view_an_internal_database_and_a_system_collection(servers: Any) -> None:
    """A `find` of a view hashes the view's namespace; mongod keeps no shape
    for a read of `admin`, `local` or `config`, or of a `system.`
    collection (measured 8.2.1, 2026-11-07)."""
    for client in servers:
        client[_DB].drop_collection("v")
        client[_DB].create_collection("v", viewOn="c", pipeline=[{"$match": {"x": {"$gt": 0}}}])
    _check(servers, SON([("find", "v"), ("filter", {"x": 1})]))
    for db, coll in [(_DB, "system.views"), ("admin", "c"), ("local", "c"), ("config", "c")]:
        for client in servers:
            reply = client[db].command(
                "explain", SON([("find", coll), ("filter", {"x": 1})]), verbosity="queryPlanner"
            )
            assert "queryShapeHash" not in reply, (db, coll)

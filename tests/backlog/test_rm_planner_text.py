"""Gap and regression tests for Rust MongoDB server items found on
2026-10-20 while reproducing mongod's query-tree text (measured against
mongod 8.2.11)."""

from __future__ import annotations

import datetime
import json
from collections.abc import Callable
from typing import Any

import pytest
from bson import Binary, Regex, json_util

from tests.backlog.conftest import mongo_answer


def _both(rust: Any, ref: Any, probe: Callable[[Any], Any]) -> tuple[Any, Any]:
    return probe(rust), probe(ref)


def _parsed(client: Any, filters: list[dict]) -> list:
    coll = client["rm195"]["c"]
    coll.drop()
    coll.insert_one({"a": 1})
    return [coll.find(f).explain()["queryPlanner"]["parsedQuery"] for f in filters]


PARSED = [
    {"a": {"$not": Regex("x")}},
    {"a": {"$not": {"$in": [1]}}},
    {"a": {"$bitsAnyClear": Binary(b"\x03", 0)}},
    {"a": 1, "$alwaysTrue": 1},
    {"a": {"$type": [2.0, "long"]}},
    {"$and": [{"a": 1}, {"a": 1.0}]},
    {"a": {"$gt": 1}, "$and": [{"a": {"$gt": 1}}]},
    {"$or": [{"a": 1}, {"a": 1}]},
    {"$or": [{"b": 1}, {"b": 1}, {"c": 2}]},
    {"$or": [{"a": Regex("x")}, {"a": Regex("y")}]},
    {"$or": [{"a": 1}, {"a": 2}, {"b": 3}, {"b": Regex("q")}]},
    {"$or": [{"a": 1}, {"a": [1]}]},
    {"x": 1, "$or": [{"a": 1}, {"a": 2}]},
    {"$or": [{"a": 1}, {"$or": [{"a": 2}, {"b": 1}]}]},
    {"a": {"$eq": Regex("x")}},
    {"a": {"$eq": Regex("x"), "$gt": 1}},
    {"a": {"$all": [Regex("x")]}},
    {"a": {"$in": [Regex("x")]}},
]


@pytest.mark.backlog_fixed("RM-195")
def test_parsed_query_follows_mongods_optimiser(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 optimises the parsed query before it plans: repeated
    clauses are one, equalities of an `$or` on one path become its `$in`
    wherever the `$or` is, a `$type` code is a whole number, `$alwaysTrue`
    drops out, and `{$eq: /x/}` stays an equality to the regex."""
    got, ref = _both(rust_mongo, mongod, lambda c: _parsed(c, PARSED))
    assert got == ref


def _bounds(client: Any) -> Any:
    coll = client["rm195"]["re"]
    coll.drop()
    coll.insert_many([{"a": "x"}, {"a": Regex("x")}])
    coll.create_index("a")
    plan = coll.find({"a": {"$eq": Regex("x")}}).explain()["queryPlanner"]["winningPlan"]
    while "indexBounds" not in plan:
        plan = plan["inputStage"]
    return plan["indexBounds"]


@pytest.mark.backlog_fixed("RM-195")
def test_equality_to_a_regex_bounds_one_point(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 bounds `{a: {$eq: /x/}}` over `a_1` by the one point
    `[/x/, /x/]`; a pattern would be bounded by every string as well."""
    got, ref = _both(rust_mongo, mongod, _bounds)
    assert got == ref == {"a": ["[/x/, /x/]"]}


PIPELINES = [
    [{"$match": {"a": 1}}, {"$sort": {"b": 1}}, {"$project": {"a": 1}}, {"$limit": 3}],
    [{"$match": {"a": 1}}, {"$project": {"a": 1}}, {"$limit": 3}],
    [{"$project": {"a": 1}}, {"$skip": 1}],
    [{"$match": {"a": {"$gt": 0}}}, {"$project": {"a": 1, "_id": 0}}, {"$sort": {"a": 1}}],
]


def _pushed(client: Any) -> list:
    db = client["rm195"]
    db.drop_collection("agg")
    db.agg.insert_many([{"a": i, "b": i % 3} for i in range(5)])
    out = []
    for p in PIPELINES:
        e = db.command(
            "explain", {"aggregate": "agg", "pipeline": p, "cursor": {}}, verbosity="queryPlanner"
        )
        stages = e.get("stages")
        planner = stages[0]["$cursor"]["queryPlanner"] if stages else e["queryPlanner"]
        out.append((planner["winningPlan"], [next(iter(s)) for s in (stages or [])[1:]]))
    return out


@pytest.mark.backlog_fixed("RM-195")
def test_aggregate_takes_a_projection_and_the_paging_behind_it(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 moves a `$skip` / `$limit` ahead of the `$project` in
    front of it, and takes a projection into the query wherever it stands,
    leaving the stages after it to the pipeline."""
    got, ref = _both(rust_mongo, mongod, _pushed)
    assert got == ref


TREES = [
    {"a": 1},
    {"a": {"$gt": 1}, "b": "x"},
    {"$or": [{"a": 1}, {"b": {"$in": [1, 2]}}]},
    {"a": {"$exists": False}},
    {"a": {"$ne": 3}},
    {"a": {"$regex": "^x", "$options": "i"}},
    {"a.b": {"$elemMatch": {"c": 1}}},
    {"a": {"$nin": [1, 2]}},
    {"a": {"$size": 2}},
    {"a": {"$type": "string"}},
    {"a": {"$mod": [2, 0]}},
    {"a": {"$lte": 5, "$gte": 1}},
    {"a": [1, 2]},
    {"a": 1.5e20},
    {"a": datetime.datetime(2020, 1, 2, 3, 4, 5)},
    {"$expr": {"$eq": ["$a", 1]}},
    {"a": {"$elemMatch": {"$gt": 1, "$lt": 5}}},
    {"a": {"$bitsAllSet": [1, 5]}},
    {"a": {"$in": ["b", "a", 3, None]}},
]


def _refusals(client: Any, run: Callable[[Any], Any]) -> list:
    db = client["rm035"]
    db.drop_collection("c")
    db.c.insert_one({"a": 1})
    client.admin.command("setParameter", 1, notablescan=True)
    try:
        return [mongo_answer(lambda f=f: run(db.c, f)) for f in TREES]
    finally:
        client.admin.command("setParameter", 1, notablescan=False)


@pytest.mark.backlog_fixed("RM-035")
def test_notablescan_refusal_prints_the_query_tree(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a collection scan under `notablescan` with its
    planner's text: `ns=<ns>Tree: <the query tree>\\nSort: ...\\nProj: ...`."""

    def run(coll: Any, f: dict) -> Any:
        return list(coll.find(f))

    got, ref = _both(rust_mongo, mongod, lambda c: _refusals(c, run))
    assert got == ref


def _commands_refused(client: Any) -> list:
    db = client["rm035c"]
    db.drop_collection("c")
    db.c.insert_one({"a": 1})
    f = {"a": 1}
    commands = [
        {"find": "c", "filter": f, "sort": {"b": 1}, "projection": {"a": 1}, "skip": 2, "limit": 3},
        {"find": "c", "filter": f, "collation": {"locale": "fr", "strength": 2}},
        {"count": "c", "query": f, "skip": 1, "limit": 3},
        {"distinct": "c", "key": "a", "query": f},
        {"aggregate": "c", "pipeline": [{"$match": f}], "cursor": {}},
        {
            "aggregate": "c",
            "pipeline": [{"$match": f}],
            "cursor": {},
            "collation": {"locale": "en"},
        },
        {"findAndModify": "c", "query": f, "update": {"$set": {"z": 1}}, "sort": {"b": 1}},
        {"findAndModify": "c", "query": f, "remove": True},
    ]
    client.admin.command("setParameter", 1, notablescan=True)
    try:
        return [mongo_answer(lambda c=c: db.command(c)) for c in commands]
    finally:
        client.admin.command("setParameter", 1, notablescan=False)


@pytest.mark.backlog_fixed("RM-035")
def test_notablescan_refusal_prints_what_each_command_asked(rust_mongo: Any, mongod: Any) -> None:
    """What each command adds to the tree (mongod 8.2.11): a find's paging,
    projection and collation as given; an aggregate's resolved collation,
    `simple` when it names none; `limit=1` and the sort for a sorted
    findAndModify; nothing for count or distinct."""
    got, ref = _both(rust_mongo, mongod, _commands_refused)
    assert got == ref


def _hint_refusals(client: Any) -> list:
    db = client["rm051h"]
    db.drop_collection("c")
    db.c.insert_one({"a": 1})
    h = {"nope": 1}
    f = {"a": 1}
    commands = [
        {"find": "c", "filter": f, "hint": "nope"},
        {
            "find": "c",
            "filter": f,
            "hint": h,
            "collation": {"locale": "fr"},
            "projection": {"a": 1},
            "skip": 2,
            "limit": 3,
        },
        {"count": "c", "query": f, "hint": h, "limit": 3},
        {"distinct": "c", "key": "a", "query": f, "hint": h},
        {
            "aggregate": "c",
            "pipeline": [{"$match": f}, {"$sort": {"b": 1}}, {"$limit": 3}],
            "hint": h,
            "cursor": {},
        },
        {"aggregate": "c", "pipeline": [{"$group": {"_id": "$a"}}], "hint": h, "cursor": {}},
        {
            "findAndModify": "c",
            "query": f,
            "update": {"$set": {"z": 1}},
            "hint": h,
            "sort": {"b": 1},
        },
        {"findAndModify": "c", "query": f, "update": {"$set": {"z": 1}}, "hint": h},
        {"explain": {"find": "c", "filter": f, "hint": h}},
    ]
    out = [mongo_answer(lambda c=c: db.command(c)) for c in commands]
    for write in (
        {"delete": "c", "deletes": [{"q": f, "limit": 1, "hint": h}]},
        {
            "update": "c",
            "updates": [{"q": f, "u": {"$set": {"z": 1}}, "hint": h, "sort": {"b": 1}}],
        },
    ):
        out.append([e["errmsg"] for e in db.command(write).get("writeErrors", [])])
    return out


@pytest.mark.backlog_fixed("RM-051")
def test_unknown_hint_is_refused_in_the_planners_words(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a hint naming no index from its planner, with
    the query each command made: `error processing query: ns=<ns>Tree: ...
    planner returned error :: caused by :: hint provided does not correspond
    to an existing index`."""
    got, ref = _both(rust_mongo, mongod, _hint_refusals)
    assert got == ref


def _time_series_hints(client: Any) -> list:
    db = client["rm089t"]
    db.drop_collection("ts")
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    at = datetime.datetime(2023, 1, 1)
    db.ts.insert_one({"t": at, "v": 1, "m": {"k": 1}})
    filters = [
        {"t": {"$gt": at}},
        {"t": at},
        {"t": {"$lte": at}},
        {"v": 1},
        {"v": {"$in": [1, 2]}},
        {"v": {"$exists": True}},
        {"v": None},
        {"m.k": {"$gt": 1}},
        {"$or": [{"v": 1}, {"m.k": 2}]},
    ]
    return [mongo_answer(lambda f=f: list(db.ts.find(f).hint({"nope": 1}))) for f in filters]


@pytest.mark.backlog_fixed("RM-089")
def test_time_series_hint_refusal_prints_the_bucket_query(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans a time-series read over its buckets: a refused
    hint prints the bucket-level predicates it derived (`control.min` /
    `control.max`, the bucket `_id` for the time field, `meta`)."""
    got, ref = _both(rust_mongo, mongod, _time_series_hints)
    assert got == ref


SCHEMAS: list[dict] = [
    {"required": ["a"]},
    {"required": ["a", "b"]},
    {"type": "array"},
    {"properties": {"a": {"type": "number"}}},
    {"properties": {"a": {"bsonType": ["int", "long"]}}},
    {"properties": {"a": {"minimum": 2}}},
    {"properties": {"a": {"maximum": 2, "exclusiveMaximum": True}}},
    {"properties": {"a": {"enum": [1, "x"]}}},
    {"properties": {"a": {"minLength": 1, "maxLength": 3}}},
    {"properties": {"a": {"pattern": "^x"}}},
    {"properties": {"a": {"multipleOf": 2.5}}},
    {"properties": {"a": {"uniqueItems": True}}},
    {"properties": {"a": {"items": {"type": "number"}}}},
    {"properties": {"a": {"items": [{"type": "number"}], "additionalItems": {"type": "string"}}}},
    {"properties": {"a": {"properties": {"b": {"type": "number"}}}}},
    {
        "properties": {
            "a": {"bsonType": "object", "required": ["x"], "properties": {"x": {"type": "string"}}}
        },
        "required": ["a"],
    },
    {"properties": {"a": {"type": "number", "minimum": 1, "maximum": 9}}},
    {"properties": {"a": {"type": "string", "minimum": 1}}},
    {"properties": {"a": {"bsonType": ["int", "long"], "minimum": 1}}},
    {"properties": {"a": {"oneOf": [{"type": "number"}, {"type": "string"}]}}},
    {"minProperties": 1},
    {"additionalProperties": False},
    {"patternProperties": {"^x": {"type": "number"}}},
    {"not": {"required": ["a"]}},
    {"anyOf": [{"required": ["a"]}, {"required": ["b"]}]},
    {"oneOf": [{"required": ["a"]}, {"required": ["b"]}]},
    {"dependencies": {"a": ["b"]}},
    {"properties": {"a": {"type": "number"}}, "required": ["a"]},
]


def _json_schema(client: Any) -> list:
    coll = client["rm196"]["c"]
    coll.drop()
    coll.insert_one({"a": 1})
    out = []
    for schema in SCHEMAS:
        parsed = coll.find({"$jsonSchema": schema}).explain()["queryPlanner"]["parsedQuery"]
        client.admin.command("setParameter", 1, notablescan=True)
        try:
            tree = mongo_answer(lambda schema=schema: list(coll.find({"$jsonSchema": schema})))
        finally:
            client.admin.command("setParameter", 1, notablescan=False)
        out.append(json.loads(json_util.dumps((parsed, tree))))
    return out


@pytest.mark.backlog_fixed("RM-196")
def test_json_schema_is_parsed_as_mongod_parses_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 translates `$jsonSchema` into the predicates it stands
    for (`{required: ["a"]}` parses as `{a: {$exists: true}}`, a property's
    keywords hold only where they apply, ...), and prints them in
    `parsedQuery` and in its planner's query tree."""
    got, ref = _both(rust_mongo, mongod, _json_schema)
    assert got == ref

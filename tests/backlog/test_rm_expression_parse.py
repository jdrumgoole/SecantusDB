"""Expressions and projections mongod refuses at PARSE time, found on
2026-10-23 by probing every position that parses an expression with a malformed
one (`{$add: [1], $b: 2}` and its relatives). mongod parses a whole command
before it reads a document, so each refusal holds on an empty collection too;
the Rust server read most of these shapes as literal values.

Each test runs the same commands on mongod live and on the Rust server and
compares the answers: documents as canonical extended JSON, errors by code and
message.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import json_util
from bson.json_util import JSONMode, JSONOptions

from tests.backlog.conftest import mongo_answer

_CANON = JSONOptions(json_mode=JSONMode.CANONICAL)
_DOC = {
    "_id": 1,
    "x": 1,
    "a": {"b": 5, "c": 2, "d": {"e": 1, "f": 2}},
    "arr": [{"b": 1, "c": 2}, {"b": 3}],
}
TWO = {"$add": [1], "$b": 2}


def _answer(client: Any, op: str, arg: Any) -> Any:
    db = client.rm_expression_parse
    db.c.drop()
    db.c.insert_one(_DOC)
    db.e.drop()
    db.create_collection("e")

    def run() -> Any:
        if op == "agg":
            return list(db.c.aggregate(arg))
        if op == "agg-empty":
            return list(db.e.aggregate(arg))
        if op == "find":
            return list(db.c.find(*arg))
        if op == "update":
            return db.c.update_one({}, arg).modified_count
        if op == "fam":
            return db.c.find_one_and_update({}, {"$set": {"q": 1}}, projection=arg)
        raise AssertionError(op)

    got = mongo_answer(run)
    if got[0] == "ok":
        return ("ok", json_util.dumps(got[1], json_options=_CANON))
    return got


def _diverging(rust_mongo: Any, mongod: Any, shapes: list) -> list:
    out = []
    for label, op, arg in shapes:
        ours, theirs = _answer(rust_mongo, op, arg), _answer(mongod, op, arg)
        if ours != theirs:
            out.append(f"{label}:\n  mongod {theirs!r}\n  rust   {ours!r}")
    return out


@pytest.mark.backlog_fixed("RM-281")
def test_two_operator_object_everywhere(rust_mongo: Any, mongod: Any) -> None:
    """**An object with an operator and another field is not a value.**
    `Expression::parseObject` takes an object whose first field starts with
    `$` as one operator (15983 otherwise), and any other object as a literal
    whose field names may not start with `$` (16410). The code depends on the
    parser that meets the object first: 40181 from `$addFields`' validator,
    16410 from the projection parser, 17390 for an inclusion-style `$group`
    `_id`. Measured 8.2.11, 2026-10-23."""
    later = {"a": 1, "$b": 2}
    shapes = [
        ("$set", "agg", [{"$set": {"y": TWO}}]),
        ("$addFields in array", "agg", [{"$addFields": {"y": [1, TWO]}}]),
        ("$addFields in $cond", "agg", [{"$addFields": {"y": {"$cond": [True, TWO, 1]}}}]),
        (
            "$addFields in $let",
            "agg",
            [{"$addFields": {"y": {"$let": {"vars": {"v": TWO}, "in": 1}}}}],
        ),
        ("$addFields in $map", "agg", [{"$addFields": {"y": {"$map": {"input": [1], "in": TWO}}}}]),
        (
            "$addFields in $switch",
            "agg",
            [
                {
                    "$addFields": {
                        "y": {"$switch": {"branches": [{"case": TWO, "then": 1}], "default": 0}}
                    }
                }
            ],
        ),
        ("$addFields $literal", "agg", [{"$addFields": {"y": {"$literal": TWO}}}]),
        ("$addFields literal $ field", "agg", [{"$addFields": {"y": {"$eq": [later, 1]}}}]),
        ("$addFields literal in array", "agg", [{"$addFields": {"y": [later]}}]),
        ("$project in array", "agg", [{"$project": {"y": [TWO]}}]),
        ("$project in $cond", "agg", [{"$project": {"y": {"$cond": [True, TWO, 1]}}}]),
        ("$replaceWith", "agg", [{"$replaceWith": TWO}]),
        ("$replaceWith literal $ field", "agg", [{"$replaceWith": later}]),
        ("$replaceRoot", "agg", [{"$replaceRoot": {"newRoot": TWO}}]),
        ("$redact", "agg", [{"$redact": TWO}]),
        ("$expr", "agg", [{"$match": {"$expr": TWO}}]),
        ("$expr unknown first", "agg", [{"$match": {"$expr": {"$nosuch": 1, "$b": 2}}}]),
        (
            "$expr two before unknown",
            "agg",
            [{"$match": {"$expr": {"$and": [TWO, {"$nosuch": 1}]}}}],
        ),
        (
            "$expr unknown before two",
            "agg",
            [{"$match": {"$expr": {"$and": [{"$nosuch": 1}, TWO]}}}],
        ),
        ("$group _id literal", "agg", [{"$group": {"_id": later}}]),
        (
            "$lookup let",
            "agg",
            [{"$lookup": {"from": "c", "let": {"v": TWO}, "pipeline": [], "as": "o"}}],
        ),
        (
            "$lookup pipeline",
            "agg",
            [{"$lookup": {"from": "c", "pipeline": [{"$addFields": {"y": TWO}}], "as": "o"}}],
        ),
        (
            "$unionWith pipeline",
            "agg",
            [{"$unionWith": {"coll": "c", "pipeline": [{"$addFields": {"y": TWO}}]}}],
        ),
        ("$facet", "agg", [{"$facet": {"f": [{"$addFields": {"y": TWO}}]}}]),
        (
            "$setWindowFields",
            "agg",
            [{"$setWindowFields": {"sortBy": {"x": 1}, "output": {"y": {"$sum": TWO}}}}],
        ),
        ("empty collection", "agg-empty", [{"$match": {"$expr": TWO}}]),
        ("find $expr", "find", [{"$expr": {"$eq": [TWO, 1]}}]),
        ("find projection", "find", [{}, {"y": TWO}]),
        ("find projection array", "find", [{}, {"y": [TWO]}]),
        ("update pipeline", "update", [{"$set": {"y": TWO}}]),
        ("update pipeline array", "update", [{"$set": {"y": [TWO]}}]),
        ("findAndModify projection", "fam", {"y": TWO}),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-282")
def test_add_fields_spec_validation(rust_mongo: Any, mongod: Any) -> None:
    """**`$addFields` / `$set` checked none of its own structure.** mongod's
    `ProjectionSpecValidator` runs over the whole spec before any expression
    is parsed: two paths that conflict (40176), an object holding an operator
    and anything else (40181, naming the whole spec), a dotted name inside a
    sub-object (40183), and a top-level name that is no field path (40352,
    40353, 15998). The Rust server applied them all, the later path
    overwriting the earlier. Measured 8.2.11, 2026-10-23."""
    shapes = [
        ("conflict", "agg", [{"$addFields": {"a": 1, "a.b": 2}}]),
        ("conflict reversed", "agg", [{"$addFields": {"a.b": 1, "a": 2}}]),
        ("conflict nested", "agg", [{"$addFields": {"a": {"b": 1}, "a.b": 2}}]),
        ("conflict with expression", "agg", [{"$addFields": {"y": {"$add": [1]}, "y.z": 1}}]),
        ("operator and field", "agg", [{"$addFields": {"y": {"$add": [1], "b": 2}}}]),
        ("field then operator", "agg", [{"$addFields": {"y": {"a": 1, "b": 2, "$c": 3}}}]),
        ("nested operator and field", "agg", [{"$addFields": {"y": {"a": {"q": 1, "$c": 3}}}}]),
        ("validator before parse", "agg", [{"$addFields": {"y": {"$nosuch": 1}, "z": TWO}}]),
        ("dotted in sub-object", "agg", [{"$addFields": {"y": {"p.q": 1}}}]),
        ("empty name", "agg", [{"$addFields": {"": 1}}]),
        ("trailing dot", "agg", [{"$addFields": {"a.": 1}}]),
        ("empty component", "agg", [{"$addFields": {"a..b": 1}}]),
        ("empty sub-object", "agg", [{"$addFields": {"y": {}}}]),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-284")
def test_projection_parse(rust_mongo: Any, mongod: Any) -> None:
    """**A projection was parsed by neither mongod's rules nor its order.**
    mongod's projection parser builds a tree as it goes: a path that lands on
    a leaf collides (31249 / 31250), an empty sub-projection is 51270, a
    child name is a field path (16410, 15998, 40352, 40353), inclusion and
    exclusion may not mix (31253, 31254) and neither may an exclusion and an
    expression (31252) or a literal (31310). The Rust `$project` answered
    most of these "not supported" and accepted the rest; `find` caught only
    some, and counted `{"a.b": 1, a: {c: 1}}` -- one sub-tree -- as a
    collision. Measured 8.2.11, 2026-10-23."""
    specs = {
        "operator and field": {"y": {"$add": [1], "b": 1}},
        "field then operator": {"y": {"a": 1, "$b": 2}},
        "operator child object": {"y": {"a": 1, "$b": {"c": 1}}},
        "two unknown": {"y": {"$nosuch": 1, "$b": 2}},
        "empty name": {"": 1},
        "trailing dot": {"a.": 1},
        "empty component": {"a..b": 1},
        "$ name": {"$y": "x"},
        "collide": {"a": 1, "a.b": 1},
        "collide reversed": {"a.b": 1, "a": 1},
        "collide nested": {"a": {"b": 1}, "a.b": 1},
        "one sub-tree": {"a.b": 1, "a": {"c": 1}},
        "inclusion then exclusion": {"a": 1, "b": 0},
        "exclusion then inclusion": {"b": 0, "a": 1},
        "exclusion then expression": {"a": 0, "b": {"$add": [1]}},
        "exclusion then nested expression": {"a": 0, "b": {"c": {"$add": [1]}}},
        "exclusion then literal": {"a": 0, "b": "x"},
        "exclusion then array": {"a": 0, "b": [1]},
        "literal then exclusion": {"b": "x", "a": 0},
        "exclusion then two operators": {"a": 0, "b": TWO},
        "empty nested": {"a": {"b": {}}},
        "DBRef field": {"y": {"$id": 1}},
    }
    shapes = []
    for label, spec in specs.items():
        shapes.append((f"$project {label}", "agg", [{"$project": spec}]))
        shapes.append((f"find {label}", "find", [{}, spec]))
    shapes += [
        ("$project empty sub-projection", "agg", [{"$project": {"y": {}}}]),
        ("$project $elemMatch", "agg", [{"$project": {"y": {"$elemMatch": {"a": 1}}}}]),
        ("$project positional", "agg", [{"$project": {"a.$": 1}}]),
        ("update pipeline $project", "update", [{"$project": {"y": TWO}}]),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-285")
def test_nested_sub_projection(rust_mongo: Any, mongod: Any) -> None:
    """**`$project: {a: {b: 1}}` returned `a: {b: 1}`, whatever `a.b` held.**
    A sub-document in a projection is a sub-projection, one leaf per field:
    `{a: {b: 1}}` includes `a.b`, `{a: {b: 0}}` excludes it. The Rust
    `$project` computed the sub-document as a literal value, so an inclusion
    returned the flag and an exclusion returned only `{a: {b: 0}}`; `find`
    refused every nested projection as "not supported". Measured 8.2.11,
    2026-10-23."""
    specs = {
        "inclusion": {"a": {"b": 1}},
        "inclusion true": {"a": {"b": True}},
        "inclusion order": {"a": {"c": 1, "b": 1}},
        "exclusion": {"a": {"b": 0}},
        "deep exclusion": {"a": {"d": {"e": 0}}},
        "exclusion and _id": {"_id": 0, "a": {"c": 0}},
        "inclusion and literal": {"a": {"b": 1, "z": "$x"}},
        "dotted inside": {"a": {"d.e": 1}},
        "array of documents": {"arr": {"b": 1}},
        "number 2 includes": {"a": 2},
    }
    shapes = []
    for label, spec in specs.items():
        shapes.append((f"$project {label}", "agg", [{"$project": spec}]))
        shapes.append((f"find {label}", "find", [{}, spec]))
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog("RM-286")
def test_meta_in_exclusion_project(rust_mongo: Any, mongod: Any) -> None:
    """`$meta` is the one expression an exclusion `$project` may hold; with
    no text query behind it, mongod answers 40218. The Rust server answers
    "not supported" (measured 8.2.11, 2026-10-23)."""
    shapes = [
        ("exclusion then $meta", "agg", [{"$project": {"a": 0, "b": {"$meta": "textScore"}}}]),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog("RM-287")
def test_find_only_projection_operators(rust_mongo: Any, mongod: Any) -> None:
    """`find`'s own projection forms at their edges. `{$slice: [<path>, n]}`
    is not find's `[skip, limit]`, so mongod parses it as the expression
    `$slice` and fails per document (28724); a positional `a.$` whose array
    the filter never names is accepted when `a` is not an array. The Rust
    server ignores the first and refuses the second with a generic code 2
    (measured 8.2.11, 2026-10-23)."""
    shapes = [
        ("$slice with a path", "find", [{}, {"y": {"$slice": ["$a", 1]}}]),
        ("positional, empty filter", "find", [{}, {"a.$": 1}]),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []

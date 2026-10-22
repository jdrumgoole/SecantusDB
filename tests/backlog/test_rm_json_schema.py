"""Regression tests for the `$jsonSchema` divergences found on 2026-10-22 by
`tools/probes/query_shapes.py` (its `schema ` cases), and fixed by porting
mongod's `JSONSchemaParser` (`secantus-core/src/jsonschema.rs`). Each test
asks mongod live and compares the matched `_id`s, or the error's code and
message.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Decimal128, Int64

from tests.backlog.conftest import mongo_answer

NAN = float("nan")
INF = float("inf")


def _answer(client: Any, schema: Any, docs: list) -> Any:
    coll = client.rm_json_schema.c
    coll.drop()
    coll.insert_many(docs)
    got = mongo_answer(
        lambda: [d["_id"] for d in coll.find({"$jsonSchema": schema}).sort("_id", 1)]
    )
    return got[1] if got[0] == "ok" else got


def _diverging(rust_mongo: Any, mongod: Any, docs: list, shapes: list[tuple]) -> list[str]:
    out = []
    for label, schema in shapes:
        ours, theirs = _answer(rust_mongo, schema, docs), _answer(mongod, schema, docs)
        if ours != theirs:
            out.append(f"{label}: mongod {theirs!r}, rust {ours!r}")
    return out


DOCS = [
    {"_id": 1, "a": {"b": 1}, "n": 0.3, "u": [{"x": 1, "y": 2}, {"y": 2, "x": 1}]},
    {"_id": 2, "a": {}, "n": 5, "u": [1, 2]},
    {"_id": 3, "a.b": 1, "n": NAN, "u": [1, 1.0]},
    {"_id": 4, "a": {"b": "x"}, "n": Decimal128("NaN"), "u": [[1, 2], [1, 2]]},
    {"_id": 5, "a": {"c": 1, "b": 1}, "n": INF, "u": []},
    {
        "_id": 6,
        "a": {"b": 1, "c": 1},
        "n": Int64(15),
        "u": [{"x": {"p": 1, "q": 2}}, {"x": {"q": 2, "p": 1}}],
    },
    {"_id": 7, "a": [{"b": 1}], "n": Decimal128("0.3"), "u": [1]},
    {"_id": 8, "n": -INF},
]


@pytest.mark.backlog_fixed("RM-268")
def test_json_schema_answers(rust_mongo: Any, mongod: Any) -> None:
    """**$jsonSchema answers differently from mongod.** mongod reads the names
    in `required` and `properties` as dotted paths; compares documents in
    `enum` and `uniqueItems` regardless of field order; computes `multipleOf`
    in Decimal128; and treats NaN and the infinities in `minimum` /
    `maximum` by its own rule (measured 8.2.11, 2026-10-22). Fixed by
    translating the schema into mongod's own match-expression tree."""
    shapes = [
        ("required dotted", {"required": ["a.b"]}),
        ("required deep dotted", {"required": ["a.b.c"]}),
        ("properties dotted name", {"properties": {"a.b": {"bsonType": "int"}}}),
        ("enum document field order", {"properties": {"a": {"enum": [{"c": 1, "b": 1}]}}}),
        ("uniqueItems field order", {"properties": {"u": {"uniqueItems": True}}}),
        ("multipleOf 0.1", {"properties": {"n": {"multipleOf": 0.1}}}),
        ("multipleOf 5", {"properties": {"n": {"multipleOf": 5}}}),
        ("minimum 5", {"properties": {"n": {"minimum": 5}}}),
        ("maximum 5 exclusive", {"properties": {"n": {"maximum": 5, "exclusiveMaximum": True}}}),
        ("minimum inf", {"properties": {"n": {"minimum": INF}}}),
        ("not minimum", {"properties": {"n": {"not": {"minimum": 5}}}}),
    ]
    assert _diverging(rust_mongo, mongod, DOCS, shapes) == []


@pytest.mark.backlog_fixed("RM-269")
def test_json_schema_validation(rust_mongo: Any, mongod: Any) -> None:
    """**A malformed $jsonSchema is not refused.** mongod parses the whole
    schema first and refuses an unknown or unsupported keyword, a keyword
    argument of the wrong type or range, and a contradiction (`type` with
    `bsonType`), each by its own code and message; the Rust server matched
    documents, or answered "not supported" (measured 8.2.11, 2026-10-22)."""
    shapes = [
        ("unknown keyword", {"bogus": 1}),
        ("unsupported keyword", {"$ref": "#/x"}),
        ("bsonType unknown alias", {"properties": {"a": {"bsonType": "foo"}}}),
        ("bsonType integer", {"properties": {"a": {"bsonType": "integer"}}}),
        ("bsonType number code", {"properties": {"a": {"bsonType": 16}}}),
        ("bsonType empty list", {"properties": {"a": {"bsonType": []}}}),
        ("bsonType duplicate", {"properties": {"a": {"bsonType": ["int", "int"]}}}),
        ("type int", {"properties": {"a": {"type": "int"}}}),
        ("type non-string", {"properties": {"a": {"type": 1}}}),
        ("type and bsonType", {"properties": {"a": {"type": "object", "bsonType": "object"}}}),
        ("required empty", {"required": []}),
        ("required non-string", {"required": [1]}),
        ("enum empty", {"properties": {"a": {"enum": []}}}),
        ("enum duplicate", {"properties": {"a": {"enum": [1, 1]}}}),
        ("multipleOf zero", {"properties": {"n": {"multipleOf": 0}}}),
        ("minItems fraction", {"properties": {"u": {"minItems": 1.5}}}),
        ("maxLength negative", {"properties": {"a": {"maxLength": -1}}}),
        ("exclusiveMinimum alone", {"properties": {"n": {"exclusiveMinimum": True}}}),
        ("allOf empty", {"properties": {"a": {"allOf": []}}}),
        ("not a number", {"properties": {"a": {"not": 1}}}),
        ("pattern unbalanced", {"properties": {"a": {"pattern": "("}}}),
        ("schema not an object", 1),
    ]
    assert _diverging(rust_mongo, mongod, DOCS, shapes) == []


def _commands(client: Any, schema: Any) -> dict:
    """What each command that takes a filter answers, over an EMPTY collection."""
    db = client.rm_json_schema
    db.c.drop()
    db.create_collection("c")
    f = {"$jsonSchema": schema}
    c = db.c
    return {
        "count": mongo_answer(lambda: c.count_documents(f)),
        "distinct": mongo_answer(lambda: c.distinct("_id", f)),
        "aggregate": mongo_answer(lambda: list(c.aggregate([{"$match": f}]))),
        "update": mongo_answer(lambda: c.update_many(f, {"$set": {"z": 1}}).modified_count),
        "delete": mongo_answer(lambda: c.delete_many(f).deleted_count),
        "findAndModify": mongo_answer(lambda: c.find_one_and_update(f, {"$set": {"z": 1}})),
        "validator": _validator(db, f),
    }


def _validator(db: Any, f: dict) -> Any:
    """Creating a collection with the schema as its validator."""
    db.v.drop()
    return mongo_answer(lambda: db.create_collection("v", validator=f).name)


@pytest.mark.backlog_fixed("RM-269")
def test_json_schema_refused_by_every_command(rust_mongo: Any, mongod: Any) -> None:
    """**A malformed $jsonSchema is refused before any document is read.**
    mongod translates the schema while it parses the filter, so `count`,
    `distinct`, `aggregate`, `update`, `delete`, `findAndModify` and a
    collection validator all refuse it over an empty collection; the Rust
    server answered as if it were valid (measured 8.2.11, 2026-10-22)."""
    out = []
    for schema in (
        {"properties": {"a": {"enum": []}}},
        {"required": "a"},
        {"properties": {"n": {"multipleOf": float("nan")}}},
    ):
        ours, theirs = _commands(rust_mongo, schema), _commands(mongod, schema)
        out += [
            f"{schema} {k}: mongod {theirs[k]!r}, rust {ours[k]!r}"
            for k in theirs
            if ours[k] != theirs[k]
        ]
    assert out == []

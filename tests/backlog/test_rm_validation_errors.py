"""Regression tests for the `errInfo` of a document-validation failure (121),
found on 2026-10-22 by `tools/probes/validation_error_details.py` over the
lines of `secantus-commands/src/validation_errors.rs` that no suite executed.
Each test inserts documents that fail a validator, on mongod live and on the
Rust server, and compares the `errInfo` EXACTLY, key order included: it is a
structure drivers render, not prose.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Binary, Decimal128, json_util


def _errinfo(client: Any, validator: dict, doc: dict) -> Any:
    db = client.rm_validation_errors
    db.v.drop()
    db.create_collection("v", validator=validator)
    reply = db.command("insert", "v", documents=[doc])
    errs = reply.get("writeErrors") or []
    if not errs:
        return ("accepted",)
    return (errs[0].get("code"), json_util.dumps(errs[0].get("errInfo")))


def _diverging(rust_mongo: Any, mongod: Any, shapes: list[tuple]) -> list[str]:
    out = []
    for label, validator, doc in shapes:
        ours, theirs = _errinfo(rust_mongo, validator, doc), _errinfo(mongod, validator, doc)
        if ours != theirs:
            out.append(f"{label}:\n  mongod {theirs!r}\n  rust   {ours!r}")
    return out


def _js(**props: Any) -> dict:
    return {"$jsonSchema": {"properties": props}}


@pytest.mark.backlog_fixed("RM-270")
def test_schema_rules_judged_as_the_matcher_judges(rust_mongo: Any, mongod: Any) -> None:
    """**The `$jsonSchema` explanation named rules the document did not
    break, and missed ones it did.** It judged each keyword by its own rules:
    `multipleOf` in floating point, `enum` / `uniqueItems` with field order,
    property names as single fields, and a keyword the schema's single stated
    type rules out (`{bsonType: "int", pattern: ...}`) still tested
    (measured 8.2.11, 2026-10-22)."""
    shapes = [
        ("dotted property", _js(**{"a.b": {"bsonType": "int"}}), {"_id": 1, "a": {"b": "x"}}),
        ("dotted required", {"$jsonSchema": {"required": ["a.b"]}}, {"_id": 1, "a.b": 1}),
        ("multipleOf decimal", _js(a={"multipleOf": 0.1, "maximum": 0}), {"_id": 1, "a": 0.3}),
        (
            "enum field order",
            _js(a={"enum": [{"x": 1, "y": 2}], "bsonType": "string"}),
            {"_id": 1, "a": {"y": 2, "x": 1}},
        ),
        (
            "uniqueItems field order",
            _js(a={"uniqueItems": True}),
            {"_id": 1, "a": [{"x": 1, "y": 2}, {"y": 2, "x": 1}]},
        ),
        (
            "uniqueItems nan",
            _js(a={"uniqueItems": True}),
            {"_id": 1, "a": [float("nan"), Decimal128("NaN")]},
        ),
        (
            "stated type drops pattern",
            _js(a={"pattern": "^x", "bsonType": "int"}),
            {"_id": 1, "a": "y"},
        ),
        ("top-level scalar enum", {"$jsonSchema": {"enum": [1, 2]}}, {"_id": 1}),
        ("encrypt not binData", _js(a={"encrypt": {}}), {"_id": 1, "a": 1}),
        ("encrypt wrong subtype", _js(a={"encrypt": {}}), {"_id": 1, "a": Binary(b"x", 0)}),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-271")
def test_schema_explanation_shape(rust_mongo: Any, mongod: Any) -> None:
    """**A sub-schema's `title` and `description` were left out, and
    `additionalProperties` came after `patternProperties`.** mongod puts them
    on the entry that reports the sub-schema, and reports the two property
    keywords from one node whose first child is `additionalProperties`
    (measured 8.2.11, 2026-10-22)."""
    shapes = [
        (
            "title at top and on a property",
            {
                "$jsonSchema": {
                    "title": "T",
                    "description": "D",
                    "properties": {"a": {"title": "t", "description": "d", "bsonType": "int"}},
                }
            },
            {"_id": 1, "a": "x"},
        ),
        (
            "description on items",
            _js(a={"items": {"description": "e", "bsonType": "int"}}),
            {"_id": 1, "a": ["x"]},
        ),
        (
            "title on oneOf entries",
            _js(
                a={
                    "oneOf": [
                        {"title": "C0", "bsonType": "int"},
                        {"bsonType": "string", "title": "C1"},
                    ]
                }
            ),
            {"_id": 1, "a": 1.5},
        ),
        (
            "title on additionalProperties",
            {"$jsonSchema": {"additionalProperties": {"title": "AP", "bsonType": "int"}}},
            {"_id": 1, "b": "x"},
        ),
        (
            "additionalProperties before patternProperties",
            {
                "$jsonSchema": {
                    "properties": {"_id": {}},
                    "patternProperties": {"^x": {"bsonType": "int"}},
                    "additionalProperties": {"bsonType": "string"},
                }
            },
            {"_id": 1, "xa": "s", "b": 2, "xb": 3},
        ),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-272")
def test_query_operator_explanations(rust_mongo: Any, mongod: Any) -> None:
    """**Query-operator explanations differed in four shapes.** An array,
    bit, `$mod` or `$regex` operator over a value of no type it can match is
    "type did not match" with the expected types; a geo operator has its own
    reasons; a numeric path component reads each element's field before the
    position, and a branch missing the path ends the considered values; and
    a `$jsonSchema` under `$nor` that matched is "schema matched" (measured
    8.2.11, 2026-10-22)."""
    shapes = [
        ("$size on a number", {"a": {"$size": 0}}, {"_id": 1, "a": 5}),
        ("$elemMatch on a number", {"a": {"$elemMatch": {"$gt": 1}}}, {"_id": 1, "a": 5}),
        ("$bitsAllSet on a string", {"a": {"$bitsAllSet": 1}}, {"_id": 1, "a": "x"}),
        ("$regex on a number", {"a": {"$regex": "x"}}, {"_id": 1, "a": 5}),
        ("$geoWithin", {"a": {"$geoWithin": {"$box": [[0, 0], [1, 1]]}}}, {"_id": 1, "a": [5, 5]}),
        (
            "$geoWithin not geometry",
            {"a": {"$geoWithin": {"$box": [[0, 0], [1, 1]]}}},
            {"_id": 1, "a": "x"},
        ),
        ("positional", {"a.0": {"$gt": 5}}, {"_id": 1, "a": [1, 2]}),
        ("positional and field", {"a.0": {"$gt": 5}}, {"_id": 1, "a": [{"0": 1}, 2]}),
        ("first branch missing", {"a.b": {"$gt": 5}}, {"_id": 1, "a": [{"c": 1}, {"b": 2}]}),
        (
            "schema matched under $nor",
            {"$nor": [{"$jsonSchema": {"required": ["_id"]}}]},
            {"_id": 1},
        ),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []


@pytest.mark.backlog_fixed("RM-273")
def test_unexplainable_validator_note(rust_mongo: Any, mongod: Any) -> None:
    """**A validator holding `$alwaysTrue` / `$alwaysFalse` cannot be
    explained, and mongod says so.** Its generator needs an annotation on
    every node, and the parser gives those two none, so the write is refused
    with a `note` and the generator's own error in place of the details; the
    Rust server invented an explanation (measured 8.2.11, 2026-10-22)."""
    shapes = [
        ("$alwaysFalse", {"$alwaysFalse": 1}, {"_id": 1}),
        ("$alwaysFalse in $or", {"$or": [{"$alwaysFalse": 1}, {"a": 1}]}, {"_id": 1, "a": 2}),
        ("$alwaysTrue in $nor", {"$nor": [{"$alwaysTrue": 1}]}, {"_id": 1}),
    ]
    assert _diverging(rust_mongo, mongod, shapes) == []

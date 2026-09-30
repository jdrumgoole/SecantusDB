"""`errInfo` of a document-validation failure (121), against mongod.

A write that fails a collection's validator answers 121 with an `errInfo`
explaining WHICH rule failed. Driver test suites read it (mongo-csharp-driver's
`WriteError_details`, the unified-format `errorResponse` tests), and people read
it -- it is the only thing that says why a write was rejected. The Rust server
sent `{operatorName: "$jsonSchema"}` and nothing else for every `$jsonSchema`
failure (measured 8.2.11, 2026-09-30).

The comparison is EXACT, key order included: the explanation is a structure a
driver renders, not prose.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27055/?directConnection=true" \\
        python tools/probes/validation_error_details.py [--show]
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson import SON, Decimal128, Int64, Regex, json_util

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041/?directConnection=true")
SERVER = os.environ.get("PROBE_SERVER")


def js(**props):
    """A `$jsonSchema` with the given `properties`."""
    return {"$jsonSchema": {"properties": props}}


#: `(label, validator, [documents])` -- every document FAILS the validator.
CASES = [
    # --- $jsonSchema keywords -------------------------------------------------
    ("required", {"$jsonSchema": {"required": ["a", "b"]}}, [{"_id": 1}, {"_id": 2, "a": 1}]),
    ("bsonType", js(a={"bsonType": "int"}), [{"_id": 1, "a": "s"}, {"_id": 2, "a": 1.5}]),
    ("bsonType-list", js(a={"bsonType": ["int", "long"]}), [{"_id": 1, "a": "s"}]),
    ("type", js(a={"type": "string"}), [{"_id": 1, "a": 3}]),
    ("type-list", js(a={"type": ["string", "null"]}), [{"_id": 1, "a": 3}]),
    ("minimum", js(a={"minimum": 5}), [{"_id": 1, "a": 1}]),
    ("maximum", js(a={"maximum": 5}), [{"_id": 1, "a": 9}]),
    ("exclusiveMinimum", js(a={"minimum": 5, "exclusiveMinimum": True}), [{"_id": 1, "a": 5}]),
    ("exclusiveMaximum", js(a={"maximum": 5, "exclusiveMaximum": True}), [{"_id": 1, "a": 5}]),
    ("minLength", js(a={"minLength": 3}), [{"_id": 1, "a": "x"}]),
    ("maxLength", js(a={"maxLength": 1}), [{"_id": 1, "a": "xyz"}]),
    ("pattern", js(a={"pattern": "^a"}), [{"_id": 1, "a": "b"}]),
    ("enum", js(a={"enum": [1, 2]}), [{"_id": 1, "a": 3}]),
    ("multipleOf", js(a={"multipleOf": 3}), [{"_id": 1, "a": 4}]),
    ("minItems", js(a={"minItems": 2}), [{"_id": 1, "a": [1]}]),
    ("maxItems", js(a={"maxItems": 1}), [{"_id": 1, "a": [1, 2]}]),
    ("uniqueItems", js(a={"uniqueItems": True}), [{"_id": 1, "a": [1, 1]}]),
    ("items-schema", js(a={"items": {"bsonType": "int"}}), [{"_id": 1, "a": [1, "x", 2]}]),
    ("minProperties", {"$jsonSchema": {"minProperties": 3}}, [{"_id": 1}]),
    ("maxProperties", {"$jsonSchema": {"maxProperties": 1}}, [{"_id": 1, "a": 1}]),
    (
        "additionalProperties-false",
        {"$jsonSchema": {"properties": {"_id": {}, "a": {}}, "additionalProperties": False}},
        [{"_id": 1, "a": 1, "b": 2, "c": 3}],
    ),
    (
        "nested-properties",
        js(a={"bsonType": "object", "properties": {"b": {"bsonType": "string"}}}),
        [{"_id": 1, "a": {"b": 1}}],
    ),
    (
        "two-properties",
        js(a={"bsonType": "int"}, b={"bsonType": "string"}),
        [{"_id": 1, "a": "s", "b": 3}],
    ),
    (
        "two-rules-one-property",
        js(a={"bsonType": "int", "minimum": 5}),
        [{"_id": 1, "a": 1}, {"_id": 2, "a": "s"}],
    ),
    (
        "required-and-properties",
        {"$jsonSchema": {"required": ["a"], "properties": {"b": {"bsonType": "string"}}}},
        [{"_id": 1, "b": 3}],
    ),
    ("anyOf", {"$jsonSchema": {"anyOf": [{"required": ["a"]}, {"required": ["b"]}]}}, [{"_id": 1}]),
    (
        "allOf",
        {"$jsonSchema": {"allOf": [{"required": ["a"]}, {"required": ["b"]}]}},
        [{"_id": 1, "a": 1}],
    ),
    ("oneOf", {"$jsonSchema": {"oneOf": [{"required": ["a"]}, {"required": ["b"]}]}}, [{"_id": 1}]),
    ("not", {"$jsonSchema": {"not": {"required": ["a"]}}}, [{"_id": 1, "a": 1}]),
    ("top-bsonType", {"$jsonSchema": {"bsonType": "array"}}, [{"_id": 1}]),
    # --- query-operator validators ---------------------------------------------
    ("q-eq", {"a": 5}, [{"_id": 1, "a": 4}, {"_id": 2}]),
    ("q-gt", {"a": {"$gt": 5}}, [{"_id": 1, "a": 4}]),
    ("q-type", {"a": {"$type": "string"}}, [{"_id": 1, "a": 4}]),
    ("q-exists", {"a": {"$exists": True}}, [{"_id": 1}]),
    ("q-in", {"a": {"$in": [1, 2]}}, [{"_id": 1, "a": 3}]),
    ("q-regex", {"a": {"$regex": "^x"}}, [{"_id": 1, "a": "y"}]),
    ("q-regex-bare", {"a": Regex("^x")}, [{"_id": 1, "a": "y"}]),
    ("q-two-fields", SON([("a", {"$gt": 1}), ("b", {"$lt": 1})]), [{"_id": 1, "a": 0, "b": 5}]),
    ("q-range", {"a": {"$gt": 1, "$lt": 5}}, [{"_id": 1, "a": 9}]),
    ("q-and", {"$and": [{"a": 1}, {"b": 2}]}, [{"_id": 1, "a": 1, "b": 3}]),
    ("q-or", {"$or": [{"a": 1}, {"b": 2}]}, [{"_id": 1, "a": 3, "b": 3}]),
    ("q-nor", {"$nor": [{"a": 1}]}, [{"_id": 1, "a": 1}]),
    ("q-expr", {"$expr": {"$gt": ["$a", 5]}}, [{"_id": 1, "a": 1}]),
    (
        "q-types",
        {"a": {"$gte": Decimal128("1.5")}, "b": {"$ne": Int64(3)}},
        [{"_id": 1, "a": 1, "b": 3}],
    ),
    ("q-nin", {"a": {"$nin": [1, 2]}}, [{"_id": 1, "a": 2}]),
    ("q-exists-false", {"a": {"$exists": False}}, [{"_id": 1, "a": 2}]),
    ("q-size", {"a": {"$size": 2}}, [{"_id": 1, "a": [1]}]),
    ("q-all", {"a": {"$all": [1, 2]}}, [{"_id": 1, "a": [1]}]),
    ("q-elemMatch", {"a": {"$elemMatch": {"$gt": 5}}}, [{"_id": 1, "a": [1, 2]}]),
    ("q-not", {"a": {"$not": {"$gt": 5}}}, [{"_id": 1, "a": 9}]),
    ("q-type-array", {"a": {"$type": "string"}}, [{"_id": 1, "a": [1, 2]}]),
    ("q-eq-array", {"a": 5}, [{"_id": 1, "a": [1, 2]}]),
    ("q-gt-string", {"a": {"$gt": 5}}, [{"_id": 1, "a": "s"}]),
    ("q-dotted", {"a.b": {"$gt": 5}}, [{"_id": 1, "a": {"b": 1}}]),
    ("q-regex-options", {"a": {"$regex": "^x", "$options": "i"}}, [{"_id": 1, "a": "y"}]),
    ("q-and-or", {"$and": [{"a": 1}, {"$or": [{"b": 1}, {"c": 1}]}]}, [{"_id": 1, "a": 1}]),
    (
        "schema-and-field",
        SON([("$jsonSchema", {"required": ["a"]}), ("b", 1)]),
        [{"_id": 1, "b": 2}],
    ),
    (
        "oneOf-many",
        {"$jsonSchema": {"oneOf": [{"required": ["a"]}, {"required": ["_id"]}]}},
        [{"_id": 1, "a": 1}],
    ),
    (
        "dependencies-list",
        {"$jsonSchema": {"dependencies": {"a": ["b", "c"]}}},
        [{"_id": 1, "a": 1, "c": 2}],
    ),
    (
        "dependencies-schema",
        {"$jsonSchema": {"dependencies": {"a": {"required": ["z"]}}}},
        [{"_id": 1, "a": 1}],
    ),
    ("enum-then-type", js(a={"enum": [1, 2], "bsonType": "int"}), [{"_id": 1, "a": "x"}]),
    (
        "items-array",
        js(a={"items": [{"bsonType": "int"}, {"bsonType": "string"}]}),
        [{"_id": 1, "a": [1, 2]}],
    ),
    ("additionalItems", js(a={"additionalItems": False, "items": [{}]}), [{"_id": 1, "a": [1, 2]}]),
    (
        "patternProperties",
        {"$jsonSchema": {"patternProperties": {"^x": {"bsonType": "int"}}}},
        [{"_id": 1, "xa": "s"}],
    ),
    (
        "additionalProperties-schema",
        {"$jsonSchema": {"additionalProperties": {"bsonType": "int"}}},
        [{"_id": 1, "q": "s"}],
    ),
    ("top-enum", {"$jsonSchema": {"enum": [{"_id": 2}]}}, [{"_id": 1}]),
    (
        "nested-required",
        js(a={"properties": {"b": {}}, "required": ["b"]}),
        [{"_id": 1, "a": {"c": 1}}],
    ),
]


def outcome(db, validator, doc):
    db.v.drop()
    db.create_collection("v", validator=validator)
    reply = db.command("insert", "v", documents=[doc])
    errs = reply.get("writeErrors") or []
    if not errs:
        return ("accepted",)
    err = errs[0]
    return (err.get("code"), json_util.dumps(err.get("errInfo")))


def main() -> int:
    if not SERVER:
        print("PROBE_SERVER is required: this probe compares a running server with mongod")
        return 2
    show = "--show" in sys.argv
    mongod = pymongo.MongoClient(MONGOD).probe_validation
    ours = pymongo.MongoClient(SERVER).probe_validation
    total = bad = 0
    for label, validator, docs in CASES:
        for doc in docs:
            total += 1
            want, got = outcome(mongod, validator, doc), outcome(ours, validator, doc)
            if want != got:
                bad += 1
                print(f"DIFF {label} {doc}")
                if show:
                    print(f"  mongod: {want}\n  ours:   {got}")
    for db in (mongod, ours):
        db.client.drop_database("probe_validation")
    print(f"=== validation errInfo: {bad} of {total} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

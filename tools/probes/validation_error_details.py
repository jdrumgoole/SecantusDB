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
from bson import SON, Binary, Decimal128, Int64, Regex, json_util

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
    # --- shapes the first cases left unexecuted (2026-10-22) -------------------
    # $jsonSchema: paths, numbers and equality as the matcher reads them
    ("dotted-property", js(**{"a.b": {"bsonType": "int"}}), [{"_id": 1, "a": {"b": "x"}}]),
    (
        "dotted-property-array",
        js(**{"a.b": {"minimum": 5}}),
        [{"_id": 1, "a": [{"b": 1}, {"b": 2}]}, {"_id": 2, "a": [{"b": 1}, {"b": 9}, {"b": "x"}]}],
    ),
    ("dotted-required", {"$jsonSchema": {"required": ["a.b"]}}, [{"_id": 1, "a": {"c": 1}}]),
    ("literal-dotted-field", {"$jsonSchema": {"required": ["a.b"]}}, [{"_id": 1, "a.b": 1}]),
    ("multipleOf-decimal-rule", js(a={"multipleOf": 0.1}), [{"_id": 1, "a": 0.35}]),
    (
        "multipleOf-passes-fails-other",
        js(a={"multipleOf": 0.1, "maximum": 0}),
        [{"_id": 1, "a": 0.3}],
    ),
    ("multipleOf-decimal-value", js(a={"multipleOf": 2}), [{"_id": 1, "a": Decimal128("3.0")}]),
    ("multipleOf-nan-value", js(a={"multipleOf": 2}), [{"_id": 1, "a": float("nan")}]),
    ("minimum-decimal-bound", js(a={"minimum": Decimal128("5.5")}), [{"_id": 1, "a": 5}]),
    ("minimum-nan-value", js(a={"minimum": 0}), [{"_id": 1, "a": float("nan")}]),
    ("maximum-nan-bound", js(a={"maximum": float("nan")}), [{"_id": 1, "a": 1}]),
    (
        "enum-field-order",
        js(a={"enum": [{"x": 1, "y": 2}], "bsonType": "string"}),
        [
            {"_id": 1, "a": {"y": 2, "x": 1}},
        ],
    ),
    ("enum-document", js(a={"enum": [{"x": 1}]}), [{"_id": 1, "a": {"x": 2}}]),
    ("enum-array-value", js(a={"enum": [1]}), [{"_id": 1, "a": [1]}]),
    (
        "uniqueItems-field-order",
        js(a={"uniqueItems": True}),
        [{"_id": 1, "a": [{"x": 1, "y": 2}, {"y": 2, "x": 1}]}],
    ),
    (
        "uniqueItems-cross-type",
        js(a={"uniqueItems": True}),
        [{"_id": 1, "a": [1, Int64(1), 2.0, 2]}],
    ),
    (
        "uniqueItems-nan",
        js(a={"uniqueItems": True}),
        [{"_id": 1, "a": [float("nan"), Decimal128("NaN")]}],
    ),
    ("minLength-non-ascii", js(a={"minLength": 3}), [{"_id": 1, "a": "é\U0001f600"}]),
    ("pattern-symbol-ignored", js(a={"pattern": "^x", "bsonType": "int"}), [{"_id": 1, "a": "y"}]),
    ("type-number", js(a={"type": "number"}), [{"_id": 1, "a": "s"}]),
    ("type-boolean-list", js(a={"type": ["boolean", "null"]}), [{"_id": 1, "a": 1}]),
    ("bsonType-number-array", js(a={"bsonType": "number"}), [{"_id": 1, "a": [1, 2]}]),
    ("bsonType-object-for-array", js(a={"bsonType": "object"}), [{"_id": 1, "a": [{"b": 1}]}]),
    # nesting, combinators, titles
    (
        "items-nested-properties",
        js(a={"items": {"properties": {"x": {"minimum": 2}}, "required": ["x"]}}),
        [{"_id": 1, "a": [{"x": 3}, {"x": 1}, {"y": 1}]}],
    ),
    (
        "items-two-failing",
        js(a={"items": {"bsonType": "int"}}),
        [{"_id": 1, "a": ["x", "y"]}],
    ),
    (
        "items-array-and-additional-schema",
        js(a={"items": [{"bsonType": "int"}], "additionalItems": {"bsonType": "string"}}),
        [{"_id": 1, "a": ["x", 1, "ok", 2]}],
    ),
    (
        "items-array-shorter",
        js(a={"items": [{}, {"bsonType": "int"}]}),
        [{"_id": 1, "a": [1, "x"]}],
    ),
    (
        "title-description",
        {
            "$jsonSchema": {
                "title": "T",
                "description": "D",
                "properties": {"a": {"title": "t", "description": "d", "bsonType": "int"}},
            }
        },
        [{"_id": 1, "a": "x"}],
    ),
    (
        "description-in-items",
        js(a={"items": {"description": "each", "bsonType": "int"}}),
        [{"_id": 1, "a": ["x"]}],
    ),
    (
        "allOf-nested-property",
        js(a={"allOf": [{"bsonType": "int"}, {"minimum": 5}]}),
        [{"_id": 1, "a": 1}],
    ),
    (
        "anyOf-under-property",
        js(a={"anyOf": [{"bsonType": "string"}, {"minimum": 5}]}),
        [{"_id": 1, "a": 1}],
    ),
    ("not-under-property", js(a={"not": {"bsonType": "int"}}), [{"_id": 1, "a": 1}]),
    (
        "not-top-with-properties",
        {"$jsonSchema": {"not": {"properties": {"a": {"bsonType": "int"}}}}},
        [{"_id": 1, "a": 1}],
    ),
    (
        "oneOf-under-property-two",
        js(a={"oneOf": [{"minimum": 0}, {"maximum": 10}]}),
        [{"_id": 1, "a": 5}],
    ),
    (
        "nested-additionalProperties-false",
        js(a={"properties": {"x": {}}, "additionalProperties": False}),
        [{"_id": 1, "a": {"x": 1, "y": 2, "z": 3}}],
    ),
    (
        "patternProperties-and-additional",
        {
            "$jsonSchema": {
                "properties": {"_id": {}},
                "patternProperties": {"^x": {"bsonType": "int"}},
                "additionalProperties": {"bsonType": "string"},
            }
        },
        [{"_id": 1, "xa": "s", "b": 2, "xb": 3}],
    ),
    (
        "patternProperties-two-patterns",
        {"$jsonSchema": {"patternProperties": {"a": {"bsonType": "int"}, "b": {"minimum": 5}}}},
        [{"_id": 1, "ab": "x"}],
    ),
    (
        "nested-dependencies",
        js(a={"dependencies": {"b": ["c"], "d": {"required": ["e"]}}}),
        [{"_id": 1, "a": {"b": 1, "d": 1}}],
    ),
    (
        "nested-minProperties",
        js(a={"minProperties": 3, "maxProperties": 0}),
        [{"_id": 1, "a": {"x": 1}}],
    ),
    (
        "required-and-properties-same",
        {"$jsonSchema": {"required": ["a"], "properties": {"a": {"bsonType": "int"}}}},
        [{"_id": 1, "a": "x"}, {"_id": 2}],
    ),
    ("top-enum-scalar", {"$jsonSchema": {"enum": [1, 2]}}, [{"_id": 1}]),
    ("top-type-list", {"$jsonSchema": {"type": ["string", "array"]}}, [{"_id": 1}]),
    ("top-minimum-ignored", {"$jsonSchema": {"minimum": 5, "required": ["z"]}}, [{"_id": 1}]),
    ("encrypt-not-bindata", js(a={"encrypt": {}}), [{"_id": 1, "a": 1}]),
    ("encrypt-wrong-subtype", js(a={"encrypt": {}}), [{"_id": 1, "a": Binary(b"x", 0)}]),
    (
        "schema-in-or",
        {"$or": [{"$jsonSchema": {"required": ["a"]}}, {"b": 1}]},
        [{"_id": 1, "b": 2}],
    ),
    ("schema-in-nor", {"$nor": [{"$jsonSchema": {"required": ["_id"]}}]}, [{"_id": 1}]),
    # query-operator validators
    ("q-ne", {"a": {"$ne": 1}}, [{"_id": 1, "a": 1}]),
    ("q-lte-array", {"a": {"$lte": 0}}, [{"_id": 1, "a": [1, 2]}]),
    ("q-missing-gt", {"a": {"$gt": 1}}, [{"_id": 1}]),
    ("q-mod", {"a": {"$mod": [3, 1]}}, [{"_id": 1, "a": 5}]),
    ("q-bits", {"a": {"$bitsAllSet": 3}}, [{"_id": 1, "a": 4}]),
    ("q-bits-any-clear", {"a": {"$bitsAnyClear": [0]}}, [{"_id": 1, "a": 1}]),
    ("q-type-list", {"a": {"$type": ["string", "bool"]}}, [{"_id": 1, "a": 1}]),
    ("q-exists-false-null", {"a": {"$exists": False}}, [{"_id": 1, "a": None}]),
    ("q-eq-null-missing", {"a": {"$ne": None}}, [{"_id": 1}]),
    ("q-in-regex", {"a": {"$in": [Regex("^x"), 1]}}, [{"_id": 1, "a": "y"}]),
    ("q-nin-array", {"a": {"$nin": [1]}}, [{"_id": 1, "a": [2, 1]}]),
    ("q-all-elemMatch", {"a": {"$all": [{"$elemMatch": {"b": 1}}]}}, [{"_id": 1, "a": [{"b": 2}]}]),
    ("q-elemMatch-object", {"a": {"$elemMatch": {"b": 1, "c": 2}}}, [{"_id": 1, "a": [{"b": 1}]}]),
    ("q-size-not-array", {"a": {"$size": 0}}, [{"_id": 1, "a": 5}]),
    ("q-not-regex", {"a": {"$not": Regex("^x")}}, [{"_id": 1, "a": "xy"}]),
    ("q-not-two-ops", {"a": {"$not": {"$gt": 1, "$lt": 9}}}, [{"_id": 1, "a": 5}]),
    ("q-nor-two", {"$nor": [{"a": 1}, {"b": 1}]}, [{"_id": 1, "a": 1, "b": 1}]),
    ("q-or-nested-and", {"$or": [{"$and": [{"a": 1}, {"b": 1}]}, {"c": 1}]}, [{"_id": 1, "a": 1}]),
    ("q-and-three", {"$and": [{"a": 1}, {"b": 1}, {"c": 1}]}, [{"_id": 1, "a": 2, "c": 1}]),
    (
        "q-expr-and",
        {"$expr": {"$and": [{"$gt": ["$a", 1]}, {"$lt": ["$a", 0]}]}},
        [{"_id": 1, "a": 5}],
    ),
    (
        "q-expr-with-field",
        SON([("$expr", {"$eq": ["$a", 2]}), ("b", 1)]),
        [{"_id": 1, "a": 1, "b": 2}],
    ),
    ("q-dotted-array", {"a.b": {"$gt": 5}}, [{"_id": 1, "a": [{"b": 1}, {"b": 2}]}]),
    ("q-dotted-index", {"a.0": {"$gt": 5}}, [{"_id": 1, "a": [1, 2]}]),
    ("q-alwaysFalse", {"$alwaysFalse": 1}, [{"_id": 1}]),
    ("q-eq-document", {"a": {"b": 1}}, [{"_id": 1, "a": {"b": 2}}]),
    ("q-eq-regex-field", {"a": {"$eq": Regex("x")}}, [{"_id": 1, "a": "x"}]),
    ("q-geoWithin", {"a": {"$geoWithin": {"$box": [[0, 0], [1, 1]]}}}, [{"_id": 1, "a": [5, 5]}]),
    ("q-comment", {"a": 1, "$comment": "c"}, [{"_id": 1, "a": 2}]),
    # where a sub-schema's title / description surface
    (
        "title-in-allOf",
        js(a={"allOf": [{"title": "A0", "bsonType": "int"}, {"description": "A1", "minimum": 5}]}),
        [{"_id": 1, "a": "x"}],
    ),
    (
        "title-in-anyOf",
        js(a={"anyOf": [{"title": "B0", "bsonType": "int"}]}),
        [{"_id": 1, "a": "x"}],
    ),
    (
        "title-in-oneOf",
        js(
            a={"oneOf": [{"title": "C0", "bsonType": "int"}, {"bsonType": "string", "title": "C1"}]}
        ),
        [{"_id": 1, "a": 1.5}, {"_id": 2, "a": "s"}],
    ),
    ("title-in-not", js(a={"not": {"title": "N", "bsonType": "int"}}), [{"_id": 1, "a": 1}]),
    (
        "title-in-tuple-items",
        js(a={"items": [{"title": "T0", "bsonType": "int"}]}),
        [{"_id": 1, "a": ["x"]}],
    ),
    (
        "title-in-additionalItems",
        js(a={"items": [{}], "additionalItems": {"title": "AI", "bsonType": "int"}}),
        [{"_id": 1, "a": [1, "x"]}],
    ),
    (
        "title-in-additionalProperties",
        {"$jsonSchema": {"additionalProperties": {"title": "AP", "bsonType": "int"}}},
        [{"_id": 1, "b": "x"}],
    ),
    (
        "title-in-patternProperties",
        {"$jsonSchema": {"patternProperties": {"^b": {"title": "PP", "bsonType": "int"}}}},
        [{"_id": 1, "b": "x"}],
    ),
    (
        "title-in-dependencies",
        {"$jsonSchema": {"dependencies": {"a": {"title": "D", "required": ["z"]}}}},
        [{"_id": 1, "a": 1}],
    ),
    (
        "title-nested-properties",
        js(a={"title": "outer", "properties": {"b": {"title": "inner", "bsonType": "int"}}}),
        [{"_id": 1, "a": {"b": "x"}}],
    ),
    ("title-only-top", {"$jsonSchema": {"title": "T", "required": ["z"]}}, [{"_id": 1}]),
    # array operators over a non-array, and constant predicates
    ("q-all-not-array", {"a": {"$all": [1]}}, [{"_id": 1, "a": 2}]),
    ("q-elemMatch-not-array", {"a": {"$elemMatch": {"$gt": 1}}}, [{"_id": 1, "a": 5}]),
    ("q-size-missing", {"a": {"$size": 1}}, [{"_id": 1}]),
    ("q-alwaysFalse-in-or", {"$or": [{"$alwaysFalse": 1}, {"a": 1}]}, [{"_id": 1, "a": 2}]),
    ("q-alwaysTrue-in-nor", {"$nor": [{"$alwaysTrue": 1}]}, [{"_id": 1}]),
    (
        "q-geoIntersects",
        {"a": {"$geoIntersects": {"$geometry": {"type": "Point", "coordinates": [0, 0]}}}},
        [{"_id": 1, "a": {"type": "Point", "coordinates": [5, 5]}}],
    ),
    (
        "q-geoWithin-not-geo",
        {"a": {"$geoWithin": {"$box": [[0, 0], [1, 1]]}}},
        [{"_id": 1, "a": "x"}],
    ),
    (
        "q-not-geoWithin",
        {"a": {"$not": {"$geoWithin": {"$box": [[0, 0], [9, 9]]}}}},
        [{"_id": 1, "a": [5, 5]}],
    ),
    ("q-dotted-index-missing", {"a.5": {"$gt": 5}}, [{"_id": 1, "a": [1, 2]}]),
    ("q-dotted-index-docs", {"a.0.b": {"$gt": 5}}, [{"_id": 1, "a": [{"b": 1}, {"b": 2}]}]),
    ("q-dotted-numeric-field", {"a.0": {"$gt": 5}}, [{"_id": 1, "a": [{"0": 1}, 2]}]),
    (
        "schema-in-not-elem",
        {"$nor": [{"$jsonSchema": {"properties": {"a": {"bsonType": "int"}}}}]},
        [{"_id": 1, "a": 1}],
    ),
    (
        "q-dotted-first-element-missing",
        {"a.b": {"$gt": 5}},
        [{"_id": 1, "a": [{"c": 1}, {"b": 2}]}],
    ),
    ("q-dotted-scalar-element", {"a.b": {"$gt": 5}}, [{"_id": 1, "a": [3, {"b": 2}]}]),
    ("q-dotted-empty-array", {"a.b": {"$gt": 5}}, [{"_id": 1, "a": {"b": []}}]),
    ("q-alwaysTrue-satisfied", {"$and": [{"a": 1}, {"$alwaysTrue": 1}]}, [{"_id": 1, "a": 2}]),
    ("q-bits-string", {"a": {"$bitsAllSet": 1}}, [{"_id": 1, "a": "x"}]),
    ("q-mod-string", {"a": {"$mod": [2, 0]}}, [{"_id": 1, "a": "x"}]),
    ("q-regex-number", {"a": {"$regex": "x"}}, [{"_id": 1, "a": 5}]),
    ("q-regex-mixed-array", {"a": {"$regex": "x"}}, [{"_id": 1, "a": [5, "y"]}]),
    ("encrypt-with-bsonType", js(a={"encrypt": {"bsonType": "string"}}), [{"_id": 1, "a": 5}]),
    (
        "additionalProperties-false-and-pattern",
        {
            "$jsonSchema": {
                "properties": {"_id": {}},
                "patternProperties": {"^x": {"bsonType": "int"}},
                "additionalProperties": False,
            }
        },
        [{"_id": 1, "xa": "s", "b": 2}],
    ),
    ("q-many-ops-one-field", {"a": {"$gt": 1, "$type": "int", "$ne": 5}}, [{"_id": 1, "a": 5}]),
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

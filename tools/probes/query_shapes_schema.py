"""`query_shapes.py` cases: `$jsonSchema`."""

from __future__ import annotations

import datetime

from bson import Binary, Code, Decimal128, Int64, MinKey, ObjectId, Regex, Timestamp


def _a(schema: dict) -> dict:
    """A `$jsonSchema` that constrains field `a` only."""
    return {"$jsonSchema": {"properties": {"a": schema}}}


def _s(schema: dict) -> dict:
    """A top-level `$jsonSchema`."""
    return {"$jsonSchema": schema}


#: One value per document under `u`, for `uniqueItems`.
UNIQ: list[dict] = [
    {"_id": 1, "u": [1, 2, 3]},
    {"_id": 2, "u": [1, 1.0]},
    {"_id": 3, "u": [1, Int64(1)]},
    {"_id": 4, "u": [1, Decimal128("1.00")]},
    {"_id": 5, "u": [float("nan"), float("nan")]},
    {"_id": 6, "u": [None, None]},
    {"_id": 7, "u": ["a", "A"]},
    {"_id": 8, "u": [True, 1]},
    {"_id": 9, "u": [0, -0.0]},
    {"_id": 10, "u": [{"a": 1, "b": 2}, {"b": 2, "a": 1}]},
    {"_id": 11, "u": [{"a": 1}, {"a": 1.0}]},
    {"_id": 12, "u": [[1, 2], [1, 2]]},
    {"_id": 13, "u": [[1, 2], [2, 1]]},
    {"_id": 14, "u": [[1, [2]], [1.0, [Int64(2)]]]},
    {"_id": 15, "u": []},
    {"_id": 16, "u": "not an array"},
    {"_id": 17, "u": [MinKey(), MinKey()]},
    {"_id": 18, "u": [Regex("a", "i"), Regex("a", "")]},
    {"_id": 19, "u": [Binary(b"x", 0), Binary(b"x", 5)]},
    {"_id": 20, "u": [Timestamp(1, 1), Timestamp(1, 1)]},
    {"_id": 21, "u": [Code("x"), "x"]},
    {"_id": 22, "u": [{"a": {"b": 1}}, {"a": {"b": Decimal128("1")}}]},
    {"_id": 23, "u": [datetime.datetime(2020, 1, 1), datetime.datetime(2020, 1, 1)]},
    {"_id": 24, "u": [ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), "64b7f9a2c1d2e3f4a5b6c7d8"]},
    {"_id": 25, "u": [1e308, Decimal128("1E+308")]},
    {"_id": 26, "u": [Int64(9007199254740993), 9007199254740992.0]},
]

#: Strings of known code-point and byte lengths under `s`.
STRS: list[dict] = [
    {"_id": 1, "s": ""},
    {"_id": 2, "s": "abc"},
    {"_id": 3, "s": "ABCé"},
    {"_id": 4, "s": "\U0001f600"},
    {"_id": 5, "s": "a\U0001f600b"},
    {"_id": 6, "s": "日本語"},
    {"_id": 7, "s": "é"},
    {"_id": 8, "s": "line1\nline2"},
    {"_id": 9, "s": 12345},
    {"_id": 10, "s": ["abc"]},
]

#: Numbers of every type, and their edges, under `n`.
NUMS: list[dict] = [
    {"_id": 1, "n": 0},
    {"_id": 2, "n": 5},
    {"_id": 3, "n": 5.0},
    {"_id": 4, "n": Int64(5)},
    {"_id": 5, "n": Decimal128("5")},
    {"_id": 6, "n": 5.5},
    {"_id": 7, "n": Decimal128("5.5")},
    {"_id": 8, "n": -10},
    {"_id": 9, "n": float("nan")},
    {"_id": 10, "n": float("inf")},
    {"_id": 11, "n": float("-inf")},
    {"_id": 12, "n": Decimal128("NaN")},
    {"_id": 13, "n": 0.3},
    {"_id": 14, "n": Decimal128("0.3")},
    {"_id": 15, "n": Int64(9007199254740993)},
    {"_id": 16, "n": "5"},
    {"_id": 17, "n": [5, 6]},
    {"_id": 18, "n": -0.0},
    {"_id": 19, "n": 1e300},
    {"_id": 20, "n": Decimal128("Infinity")},
]

#: `NUMS` without the NaNs, so an ordering keyword sees every other number.
FINITE: list[dict] = [d for d in NUMS if d["_id"] not in (9, 12)]

#: Values reached through dotted names and arrays of documents.
PATHS: list[dict] = [
    {"_id": 1, "a": {"b": "x"}},
    {"_id": 2, "a": {"b": 1}},
    {"_id": 3, "a.b": 1},
    {"_id": 4, "a": [{"b": 1}, {"b": "x"}]},
    {"_id": 5, "a": [{"c": 1}]},
    {"_id": 6, "a": {"b": {"c": None}}},
    {"_id": 7, "a": 1},
]

#: Objects of varying shapes under `o`, for the object keywords.
OBJS: list[dict] = [
    {"_id": 1, "o": {}},
    {"_id": 2, "o": {"x": 1}},
    {"_id": 3, "o": {"x": 1, "y": "s"}},
    {"_id": 4, "o": {"x": "s", "y": 2, "z": None}},
    {"_id": 5, "o": {"x1": 1, "x2": 2, "q": 3}},
    {"_id": 6, "o": {"x1": "a", "y": 1}},
    {"_id": 7, "o": {"a.b": 1}},
    {"_id": 8, "o": {"": 1}},
    {"_id": 9, "o": {"é": 1}},
    {"_id": 10, "o": [{"x": 1}]},
    {"_id": 11, "o": "string"},
    {"_id": 12},
    {"_id": 13, "o": {"x": None}},
    {"_id": 14, "o": {"y": 1}},
]

#: Arrays of varying shapes under `t`, for the array keywords.
ARRS: list[dict] = [
    {"_id": 1, "t": []},
    {"_id": 2, "t": [1]},
    {"_id": 3, "t": [1, "a"]},
    {"_id": 4, "t": [1, "a", True]},
    {"_id": 5, "t": ["a", 1]},
    {"_id": 6, "t": [1, 2, 3, 4]},
    {"_id": 7, "t": [[1], [2, 3]]},
    {"_id": 8, "t": [{"x": 1}, {"x": "s"}]},
    {"_id": 9, "t": [None]},
    {"_id": 10, "t": {"0": 1}},
    {"_id": 11, "t": 7},
    {"_id": 12, "t": [1, "a", 2.5]},
]

CASES: list[tuple] = [
    # --- bsonType: every alias, alone and in arrays ---------------------------------------
    ("schema bsonType double", _a({"bsonType": "double"})),
    ("schema bsonType string", _a({"bsonType": "string"})),
    ("schema bsonType object", _a({"bsonType": "object"})),
    ("schema bsonType array", _a({"bsonType": "array"})),
    ("schema bsonType binData", _a({"bsonType": "binData"})),
    ("schema bsonType undefined", _a({"bsonType": "undefined"})),
    ("schema bsonType objectId", _a({"bsonType": "objectId"})),
    ("schema bsonType bool", _a({"bsonType": "bool"})),
    ("schema bsonType date", _a({"bsonType": "date"})),
    ("schema bsonType null", _a({"bsonType": "null"})),
    ("schema bsonType regex", _a({"bsonType": "regex"})),
    ("schema bsonType dbPointer", _a({"bsonType": "dbPointer"})),
    ("schema bsonType javascript", _a({"bsonType": "javascript"})),
    ("schema bsonType symbol", _a({"bsonType": "symbol"})),
    ("schema bsonType javascriptWithScope", _a({"bsonType": "javascriptWithScope"})),
    ("schema bsonType int", _a({"bsonType": "int"})),
    ("schema bsonType timestamp", _a({"bsonType": "timestamp"})),
    ("schema bsonType long", _a({"bsonType": "long"})),
    ("schema bsonType decimal", _a({"bsonType": "decimal"})),
    ("schema bsonType minKey", _a({"bsonType": "minKey"})),
    ("schema bsonType maxKey", _a({"bsonType": "maxKey"})),
    ("schema bsonType number", _a({"bsonType": "number"})),
    ("schema bsonType list", _a({"bsonType": ["int", "string", "null"]})),
    ("schema bsonType list with number", _a({"bsonType": ["number", "array"]})),
    ("schema bsonType top object", _s({"bsonType": "object"})),
    ("schema bsonType top array", _s({"bsonType": "array"})),
    ("schema bsonType does not descend arrays", _a({"bsonType": "int"}), [{"_id": 1, "a": [1]}]),
    # --- bsonType parse errors ------------------------------------------------------------
    ("schema bsonType unknown", _a({"bsonType": "foo"})),
    ("schema bsonType integer", _a({"bsonType": "integer"})),
    ("schema bsonType numeric code", _a({"bsonType": 16})),
    ("schema bsonType numeric code in list", _a({"bsonType": [16, "string"]})),
    ("schema bsonType empty list", _a({"bsonType": []})),
    ("schema bsonType duplicate list", _a({"bsonType": ["int", "int"]})),
    ("schema bsonType object arg", _a({"bsonType": {"x": 1}})),
    ("schema bsonType unknown in list", _a({"bsonType": ["int", "nope"]})),
    ("schema bsonType unknown empty collection", _a({"bsonType": "foo"}), []),
    # --- type: the JSON names --------------------------------------------------------------
    ("schema type string", _a({"type": "string"})),
    ("schema type number", _a({"type": "number"})),
    ("schema type object", _a({"type": "object"})),
    ("schema type array", _a({"type": "array"})),
    ("schema type boolean", _a({"type": "boolean"})),
    ("schema type null", _a({"type": "null"})),
    ("schema type list", _a({"type": ["boolean", "null"]})),
    ("schema type top object", _s({"type": "object"})),
    ("schema type int refused", _a({"type": "int"})),
    ("schema type integer refused", _a({"type": "integer"})),
    ("schema type integer in list", _a({"type": ["string", "integer"]})),
    ("schema type non-string", _a({"type": 1})),
    ("schema type non-string in list", _a({"type": ["string", 1]})),
    ("schema type empty list", _a({"type": []})),
    ("schema type duplicate list", _a({"type": ["string", "string"]})),
    ("schema type and bsonType", _a({"type": "string", "bsonType": "string"})),
    ("schema type non-string empty collection", _a({"type": 1}), []),
    # --- enum ------------------------------------------------------------------------------
    ("schema enum scalars", _a({"enum": [1, "abc", None]})),
    ("schema enum cross-type numbers", _a({"enum": [3.0, Decimal128("2.50")]})),
    ("schema enum nan", _a({"enum": [float("nan")]})),
    ("schema enum array value", _a({"enum": [[1, 2, 3]]})),
    ("schema enum array element not matched", _a({"enum": [3]})),
    ("schema enum document", _a({"enum": [{"b": 1, "c": [1, 2]}]})),
    ("schema enum document field order", _a({"enum": [{"c": [1, 2], "b": 1}]})),
    ("schema enum case", _a({"enum": ["ABC"]})),
    ("schema enum top-level whole doc", _s({"enum": [{"_id": 15}]})),
    ("schema enum not array", _a({"enum": 1})),
    ("schema enum empty", _a({"enum": []})),
    ("schema enum duplicate", _a({"enum": [1, 1]})),
    ("schema enum duplicate cross-type", _a({"enum": [1, 1.0]})),
    ("schema enum not array empty collection", _a({"enum": 1}), []),
    # --- required --------------------------------------------------------------------------
    ("schema required", _s({"required": ["a"]})),
    ("schema required two", _s({"required": ["a", "b"]})),
    ("schema required null value counts", _s({"required": ["a"]}), [{"_id": 1, "a": None}]),
    ("schema required nested", _a({"required": ["b"]})),
    ("schema required dotted", _s({"required": ["a.b"]})),
    ("schema required empty", _s({"required": []})),
    ("schema required duplicate", _s({"required": ["a", "a"]})),
    ("schema required non-string", _s({"required": [1]})),
    ("schema required string", _s({"required": "a"})),
    ("schema required empty, empty collection", _s({"required": []}), []),
    ("schema required nested non-string", _a({"required": [1]})),
    # --- properties / patternProperties / additionalProperties ---------------------------
    (
        "schema properties nested",
        _s({"properties": {"a": {"properties": {"b": {"bsonType": "int"}}}}}),
    ),
    (
        "schema properties dotted name",
        {"$jsonSchema": {"properties": {"o": {"properties": {"a.b": {"bsonType": "string"}}}}}},
        OBJS,
    ),
    (
        "schema properties empty-string name",
        {"$jsonSchema": {"properties": {"o": {"properties": {"": {"bsonType": "string"}}}}}},
        OBJS,
    ),
    ("schema properties not object", _s({"properties": 1})),
    ("schema properties sub not object", _s({"properties": {"a": 1}})),
    ("schema properties empty", _s({"properties": {}})),
    (
        "schema properties on array value",
        {"$jsonSchema": {"properties": {"o": {"properties": {"x": {"bsonType": "string"}}}}}},
        OBJS,
    ),
    (
        "schema patternProperties",
        {"$jsonSchema": {"properties": {"o": {"patternProperties": {"^x": {"bsonType": "int"}}}}}},
        OBJS,
    ),
    (
        "schema patternProperties two overlap",
        {
            "$jsonSchema": {
                "properties": {
                    "o": {"patternProperties": {"^x": {"bsonType": "int"}, "1$": {"minimum": 1}}}
                }
            }
        },
        OBJS,
    ),
    (
        "schema patternProperties unicode",
        {
            "$jsonSchema": {
                "properties": {"o": {"patternProperties": {"^\\w$": {"bsonType": "string"}}}}
            }
        },
        OBJS,
    ),
    ("schema patternProperties top", _s({"patternProperties": {"^a$": {"bsonType": "int"}}})),
    ("schema patternProperties bad regex", _s({"patternProperties": {"(": {}}})),
    ("schema patternProperties not object", _s({"patternProperties": 1})),
    ("schema patternProperties sub not object", _s({"patternProperties": {"x": 1}})),
    ("schema additionalProperties false top", _s({"additionalProperties": False})),
    (
        "schema additionalProperties false top with _id",
        _s({"properties": {"_id": {}, "a": {}}, "additionalProperties": False}),
    ),
    ("schema additionalProperties true", _s({"additionalProperties": True})),
    (
        "schema additionalProperties false nested",
        {
            "$jsonSchema": {
                "properties": {"o": {"properties": {"x": {}}, "additionalProperties": False}}
            }
        },
        OBJS,
    ),
    (
        "schema additionalProperties schema",
        {
            "$jsonSchema": {
                "properties": {
                    "o": {"properties": {"x": {}}, "additionalProperties": {"bsonType": "string"}}
                }
            }
        },
        OBJS,
    ),
    (
        "schema additionalProperties with patternProperties",
        {
            "$jsonSchema": {
                "properties": {
                    "o": {"patternProperties": {"^x": {}}, "additionalProperties": False}
                }
            }
        },
        OBJS,
    ),
    (
        "schema additionalProperties schema with pattern",
        {
            "$jsonSchema": {
                "properties": {
                    "o": {
                        "patternProperties": {"^x": {"bsonType": "int"}},
                        "additionalProperties": {"bsonType": "int"},
                    }
                }
            }
        },
        OBJS,
    ),
    ("schema additionalProperties number", _s({"additionalProperties": 1})),
    ("schema additionalProperties bad sub", _s({"additionalProperties": {"bogus": 1}})),
    # --- minProperties / maxProperties ---------------------------------------------------
    ("schema minProperties top", _s({"minProperties": 3})),
    ("schema maxProperties top", _s({"maxProperties": 1})),
    (
        "schema minProperties nested",
        {"$jsonSchema": {"properties": {"o": {"minProperties": 2}}}},
        OBJS,
    ),
    (
        "schema maxProperties nested zero",
        {"$jsonSchema": {"properties": {"o": {"maxProperties": 0}}}},
        OBJS,
    ),
    ("schema minProperties long", _s({"minProperties": Int64(2)})),
    ("schema minProperties whole double", _s({"minProperties": 2.0})),
    ("schema minProperties fractional", _s({"minProperties": 2.5})),
    ("schema minProperties negative", _s({"minProperties": -1})),
    ("schema minProperties string", _s({"minProperties": "2"})),
    ("schema maxProperties decimal", _s({"maxProperties": Decimal128("2")})),
    ("schema maxProperties huge", _s({"maxProperties": 1e19})),
    # --- items / additionalItems / minItems / maxItems -----------------------------------
    (
        "schema items single",
        {"$jsonSchema": {"properties": {"t": {"items": {"bsonType": "int"}}}}},
        ARRS,
    ),
    (
        "schema items tuple",
        {
            "$jsonSchema": {
                "properties": {"t": {"items": [{"bsonType": "int"}, {"bsonType": "string"}]}}
            }
        },
        ARRS,
    ),
    (
        "schema items tuple additionalItems false",
        {
            "$jsonSchema": {
                "properties": {"t": {"items": [{"bsonType": "int"}, {}], "additionalItems": False}}
            }
        },
        ARRS,
    ),
    (
        "schema items tuple additionalItems true",
        {
            "$jsonSchema": {
                "properties": {"t": {"items": [{"bsonType": "int"}], "additionalItems": True}}
            }
        },
        ARRS,
    ),
    (
        "schema items tuple additionalItems schema",
        {
            "$jsonSchema": {
                "properties": {"t": {"items": [{}], "additionalItems": {"bsonType": "string"}}}
            }
        },
        ARRS,
    ),
    (
        "schema additionalItems without items",
        {"$jsonSchema": {"properties": {"t": {"additionalItems": False}}}},
        ARRS,
    ),
    (
        "schema additionalItems with single items",
        {"$jsonSchema": {"properties": {"t": {"items": {}, "additionalItems": False}}}},
        ARRS,
    ),
    (
        "schema items empty tuple",
        {"$jsonSchema": {"properties": {"t": {"items": [], "additionalItems": False}}}},
        ARRS,
    ),
    (
        "schema items nested arrays",
        {"$jsonSchema": {"properties": {"t": {"items": {"items": {"bsonType": "int"}}}}}},
        ARRS,
    ),
    (
        "schema items of objects",
        {
            "$jsonSchema": {
                "properties": {"t": {"items": {"properties": {"x": {"bsonType": "int"}}}}}
            }
        },
        ARRS,
    ),
    ("schema items not object", {"$jsonSchema": {"properties": {"t": {"items": 1}}}}, ARRS),
    (
        "schema items tuple element not object",
        {"$jsonSchema": {"properties": {"t": {"items": [1]}}}},
        ARRS,
    ),
    (
        "schema additionalItems number",
        {"$jsonSchema": {"properties": {"t": {"items": [{}], "additionalItems": 1}}}},
        ARRS,
    ),
    ("schema minItems", {"$jsonSchema": {"properties": {"t": {"minItems": 2}}}}, ARRS),
    ("schema maxItems", {"$jsonSchema": {"properties": {"t": {"maxItems": 1}}}}, ARRS),
    (
        "schema minItems whole double",
        {"$jsonSchema": {"properties": {"t": {"minItems": 2.0}}}},
        ARRS,
    ),
    ("schema minItems fractional", {"$jsonSchema": {"properties": {"t": {"minItems": 1.5}}}}, ARRS),
    ("schema maxItems negative", {"$jsonSchema": {"properties": {"t": {"maxItems": -1}}}}, ARRS),
    ("schema maxItems string", {"$jsonSchema": {"properties": {"t": {"maxItems": "1"}}}}, ARRS),
    ("schema minItems long", {"$jsonSchema": {"properties": {"t": {"minItems": Int64(3)}}}}, ARRS),
    (
        "schema minItems decimal",
        {"$jsonSchema": {"properties": {"t": {"minItems": Decimal128("3")}}}},
        ARRS,
    ),
    (
        "schema minItems fractional, empty collection",
        {"$jsonSchema": {"properties": {"t": {"minItems": 1.5}}}},
        [],
    ),
    # --- uniqueItems ---------------------------------------------------------------------
    (
        "schema uniqueItems true",
        {"$jsonSchema": {"properties": {"u": {"uniqueItems": True}}}},
        UNIQ,
    ),
    (
        "schema uniqueItems false",
        {"$jsonSchema": {"properties": {"u": {"uniqueItems": False}}}},
        UNIQ,
    ),
    (
        "schema uniqueItems not bool",
        {"$jsonSchema": {"properties": {"u": {"uniqueItems": 1}}}},
        UNIQ,
    ),
    ("schema uniqueItems shared docs", _a({"uniqueItems": True})),
    # --- minimum / maximum / exclusive* ---------------------------------------------------
    ("schema minimum", {"$jsonSchema": {"properties": {"n": {"minimum": 5}}}}, NUMS),
    (
        "schema minimum exclusive",
        {"$jsonSchema": {"properties": {"n": {"minimum": 5, "exclusiveMinimum": True}}}},
        NUMS,
    ),
    (
        "schema minimum exclusive false",
        {"$jsonSchema": {"properties": {"n": {"minimum": 5, "exclusiveMinimum": False}}}},
        NUMS,
    ),
    ("schema maximum", {"$jsonSchema": {"properties": {"n": {"maximum": 5}}}}, NUMS),
    (
        "schema maximum exclusive",
        {"$jsonSchema": {"properties": {"n": {"maximum": 5, "exclusiveMaximum": True}}}},
        NUMS,
    ),
    (
        "schema minimum decimal bound",
        {"$jsonSchema": {"properties": {"n": {"minimum": Decimal128("5.5")}}}},
        NUMS,
    ),
    (
        "schema maximum long bound",
        {"$jsonSchema": {"properties": {"n": {"maximum": Int64(9007199254740992)}}}},
        NUMS,
    ),
    ("schema minimum double bound", {"$jsonSchema": {"properties": {"n": {"minimum": 0.3}}}}, NUMS),
    (
        "schema minimum nan bound",
        {"$jsonSchema": {"properties": {"n": {"minimum": float("nan")}}}},
        NUMS,
    ),
    (
        "schema maximum inf bound",
        {"$jsonSchema": {"properties": {"n": {"maximum": float("inf")}}}},
        NUMS,
    ),
    (
        "schema minimum and maximum",
        {"$jsonSchema": {"properties": {"n": {"minimum": 0, "maximum": 5}}}},
        NUMS,
    ),
    ("schema minimum shared docs", _a({"minimum": 2})),
    ("schema minimum string bound", {"$jsonSchema": {"properties": {"n": {"minimum": "5"}}}}, NUMS),
    ("schema maximum null bound", {"$jsonSchema": {"properties": {"n": {"maximum": None}}}}, NUMS),
    (
        "schema exclusiveMinimum without minimum",
        {"$jsonSchema": {"properties": {"n": {"exclusiveMinimum": True}}}},
        NUMS,
    ),
    (
        "schema exclusiveMaximum without maximum",
        {"$jsonSchema": {"properties": {"n": {"exclusiveMaximum": False}}}},
        NUMS,
    ),
    (
        "schema exclusiveMinimum numeric",
        {"$jsonSchema": {"properties": {"n": {"minimum": 1, "exclusiveMinimum": 5}}}},
        NUMS,
    ),
    (
        "schema exclusiveMinimum numeric alone",
        {"$jsonSchema": {"properties": {"n": {"exclusiveMinimum": 5}}}},
        NUMS,
    ),
    (
        "schema minimum string bound, empty collection",
        {"$jsonSchema": {"properties": {"n": {"minimum": "5"}}}},
        [],
    ),
    # --- multipleOf ----------------------------------------------------------------------
    ("schema multipleOf int", {"$jsonSchema": {"properties": {"n": {"multipleOf": 5}}}}, NUMS),
    ("schema multipleOf double", {"$jsonSchema": {"properties": {"n": {"multipleOf": 0.1}}}}, NUMS),
    ("schema multipleOf half", {"$jsonSchema": {"properties": {"n": {"multipleOf": 0.5}}}}, NUMS),
    (
        "schema multipleOf decimal",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": Decimal128("0.1")}}}},
        NUMS,
    ),
    (
        "schema multipleOf long",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": Int64(3)}}}},
        NUMS,
    ),
    ("schema multipleOf zero", {"$jsonSchema": {"properties": {"n": {"multipleOf": 0}}}}, NUMS),
    (
        "schema multipleOf negative",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": -5}}}},
        NUMS,
    ),
    ("schema multipleOf string", {"$jsonSchema": {"properties": {"n": {"multipleOf": "5"}}}}, NUMS),
    (
        "schema multipleOf nan",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": float("nan")}}}},
        NUMS,
    ),
    (
        "schema multipleOf inf",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": float("inf")}}}},
        NUMS,
    ),
    (
        "schema multipleOf decimal zero",
        {"$jsonSchema": {"properties": {"n": {"multipleOf": Decimal128("0")}}}},
        NUMS,
    ),
    ("schema multipleOf shared docs", _a({"multipleOf": 2})),
    # --- minLength / maxLength / pattern -------------------------------------------------
    ("schema minLength", {"$jsonSchema": {"properties": {"s": {"minLength": 3}}}}, STRS),
    ("schema maxLength", {"$jsonSchema": {"properties": {"s": {"maxLength": 1}}}}, STRS),
    ("schema maxLength two", {"$jsonSchema": {"properties": {"s": {"maxLength": 2}}}}, STRS),
    ("schema minLength four", {"$jsonSchema": {"properties": {"s": {"minLength": 4}}}}, STRS),
    ("schema minLength five", {"$jsonSchema": {"properties": {"s": {"minLength": 5}}}}, STRS),
    (
        "schema minLength whole double",
        {"$jsonSchema": {"properties": {"s": {"minLength": 3.0}}}},
        STRS,
    ),
    (
        "schema minLength fractional",
        {"$jsonSchema": {"properties": {"s": {"minLength": 2.5}}}},
        STRS,
    ),
    ("schema maxLength negative", {"$jsonSchema": {"properties": {"s": {"maxLength": -1}}}}, STRS),
    ("schema maxLength string", {"$jsonSchema": {"properties": {"s": {"maxLength": "3"}}}}, STRS),
    (
        "schema maxLength long",
        {"$jsonSchema": {"properties": {"s": {"maxLength": Int64(3)}}}},
        STRS,
    ),
    ("schema pattern", {"$jsonSchema": {"properties": {"s": {"pattern": "^a"}}}}, STRS),
    ("schema pattern unanchored", {"$jsonSchema": {"properties": {"s": {"pattern": "b"}}}}, STRS),
    (
        "schema pattern unicode class",
        {"$jsonSchema": {"properties": {"s": {"pattern": "^\\p{Han}+$"}}}},
        STRS,
    ),
    (
        "schema pattern dot multiline",
        {"$jsonSchema": {"properties": {"s": {"pattern": "^line2"}}}},
        STRS,
    ),
    (
        "schema pattern single codepoint",
        {"$jsonSchema": {"properties": {"s": {"pattern": "^.$"}}}},
        STRS,
    ),
    ("schema pattern shared docs", _a({"pattern": "^A"})),
    ("schema pattern bad regex", {"$jsonSchema": {"properties": {"s": {"pattern": "("}}}}, STRS),
    ("schema pattern not string", {"$jsonSchema": {"properties": {"s": {"pattern": 1}}}}, STRS),
    (
        "schema pattern regex object",
        {"$jsonSchema": {"properties": {"s": {"pattern": Regex("^a", "i")}}}},
        STRS,
    ),
    (
        "schema pattern bad regex, empty collection",
        {"$jsonSchema": {"properties": {"s": {"pattern": "("}}}},
        [],
    ),
    # --- allOf / anyOf / oneOf / not -----------------------------------------------------
    ("schema allOf", _a({"allOf": [{"bsonType": "number"}, {"minimum": 2}]})),
    ("schema anyOf", _a({"anyOf": [{"bsonType": "string"}, {"bsonType": "bool"}]})),
    ("schema oneOf", _a({"oneOf": [{"bsonType": "number"}, {"minimum": 2}]})),
    ("schema oneOf none", _a({"oneOf": [{"bsonType": "string"}, {"bsonType": "int"}]})),
    ("schema not", _a({"not": {"bsonType": "number"}})),
    ("schema not top", _s({"not": {"required": ["a"]}})),
    ("schema not empty schema", _a({"not": {}})),
    ("schema anyOf top", _s({"anyOf": [{"required": ["b"]}, {"required": ["c"]}]})),
    ("schema allOf empty", _a({"allOf": []})),
    ("schema anyOf empty", _a({"anyOf": []})),
    ("schema oneOf empty", _a({"oneOf": []})),
    ("schema allOf not array", _a({"allOf": {"bsonType": "int"}})),
    ("schema anyOf element not object", _a({"anyOf": [1]})),
    ("schema oneOf element bad keyword", _a({"oneOf": [{"bogus": 1}]})),
    ("schema not not object", _a({"not": 1})),
    ("schema not array", _a({"not": [{}]})),
    ("schema not bad keyword", _a({"not": {"bogus": 1}})),
    ("schema allOf not array, empty collection", _a({"allOf": 1}), []),
    # --- dependencies --------------------------------------------------------------------
    ("schema dependencies array", _s({"dependencies": {"a": ["b"]}})),
    ("schema dependencies schema", _s({"dependencies": {"a": {"required": ["b"]}}})),
    (
        "schema dependencies schema top type",
        _s({"dependencies": {"c": {"properties": {"a": {"bsonType": "int"}}}}}),
    ),
    (
        "schema dependencies nested",
        {"$jsonSchema": {"properties": {"o": {"dependencies": {"x": ["y"]}}}}},
        OBJS,
    ),
    ("schema dependencies empty array", _s({"dependencies": {"a": []}})),
    ("schema dependencies duplicate", _s({"dependencies": {"a": ["b", "b"]}})),
    ("schema dependencies non-string", _s({"dependencies": {"a": [1]}})),
    ("schema dependencies number", _s({"dependencies": {"a": 1}})),
    ("schema dependencies not object", _s({"dependencies": ["a"]})),
    ("schema dependencies bad sub-schema", _s({"dependencies": {"a": {"bogus": 1}}})),
    ("schema dependencies empty", _s({"dependencies": {}})),
    # --- metadata ------------------------------------------------------------------------
    ("schema title and description", _s({"title": "t", "description": "d", "required": ["a"]})),
    ("schema title not string", _s({"title": 1})),
    ("schema description not string", _a({"description": 1})),
    # --- unsupported and unknown keywords ------------------------------------------------
    ("schema $ref", _s({"$ref": "#/definitions/x"})),
    ("schema $schema", _s({"$schema": "http://json-schema.org/draft-04/schema#"})),
    ("schema default", _a({"default": 1})),
    ("schema definitions", _s({"definitions": {}})),
    ("schema format", _a({"format": "email"})),
    ("schema id", _s({"id": "x"})),
    ("schema unknown keyword", _s({"bogus": 1})),
    ("schema unknown keyword nested", _a({"bogus": 1})),
    (
        "schema unknown keyword in items",
        {"$jsonSchema": {"properties": {"t": {"items": [{"bogus": 1}]}}}},
        ARRS,
    ),
    ("schema draft-6 const", _a({"const": 1})),
    ("schema draft-6 contains", _a({"contains": {}})),
    ("schema draft-6 propertyNames", _s({"propertyNames": {}})),
    ("schema $comment keyword", _s({"$comment": "x"})),
    ("schema encrypt", _a({"encrypt": {}})),
    ("schema encryptMetadata", _s({"encryptMetadata": {}})),
    ("schema unknown keyword, empty collection", _s({"bogus": 1}), []),
    ("schema $ref, empty collection", _s({"$ref": "x"}), []),
    # --- placement of $jsonSchema --------------------------------------------------------
    ("schema not an object", {"$jsonSchema": 1}),
    ("schema array", {"$jsonSchema": [{"required": ["a"]}]}),
    ("schema empty", {"$jsonSchema": {}}),
    ("schema with field predicate", {"$jsonSchema": {"required": ["a"]}, "a": {"$type": "number"}}),
    ("schema in $and", {"$and": [{"$jsonSchema": {"required": ["b"]}}, {"a": {"$exists": True}}]}),
    ("schema in $or", {"$or": [{"$jsonSchema": {"required": ["b"]}}, {"a": 1}]}),
    ("schema in $nor", {"$nor": [{"$jsonSchema": {"required": ["a"]}}]}),
    ("schema in $or bad keyword", {"$or": [{"$jsonSchema": {"bogus": 1}}, {"a": 1}]}),
    ("schema at field level", {"a": {"$jsonSchema": {}}}),
    ("schema in $elemMatch", {"a": {"$elemMatch": {"$jsonSchema": {"required": ["b"]}}}}),
    ("schema in $not", {"a": {"$not": {"$jsonSchema": {}}}}),
    (
        "schema twice",
        {"$and": [{"$jsonSchema": {"required": ["a"]}}, {"$jsonSchema": {"required": ["b"]}}]},
    ),
    ("schema with $expr", {"$jsonSchema": {"required": ["a"]}, "$expr": {"$eq": ["$a", 1]}}),
    # --- ordering keywords over finite numbers (no NaN to defer on) ----------------------
    ("schema finite minimum", {"$jsonSchema": {"properties": {"n": {"minimum": 5}}}}, FINITE),
    (
        "schema finite minimum exclusive",
        {"$jsonSchema": {"properties": {"n": {"minimum": 5, "exclusiveMinimum": True}}}},
        FINITE,
    ),
    ("schema finite maximum", {"$jsonSchema": {"properties": {"n": {"maximum": 5}}}}, FINITE),
    (
        "schema finite maximum exclusive",
        {"$jsonSchema": {"properties": {"n": {"maximum": 5, "exclusiveMaximum": True}}}},
        FINITE,
    ),
    (
        "schema finite minimum decimal bound",
        {"$jsonSchema": {"properties": {"n": {"minimum": Decimal128("5.5")}}}},
        FINITE,
    ),
    (
        "schema finite maximum long bound",
        {"$jsonSchema": {"properties": {"n": {"maximum": Int64(9007199254740992)}}}},
        FINITE,
    ),
    (
        "schema finite minimum inf bound",
        {"$jsonSchema": {"properties": {"n": {"minimum": float("inf")}}}},
        FINITE,
    ),
    (
        "schema finite maximum -0.0 bound exclusive",
        {"$jsonSchema": {"properties": {"n": {"maximum": -0.0, "exclusiveMaximum": True}}}},
        FINITE,
    ),
    (
        "schema finite allOf",
        {"$jsonSchema": {"properties": {"n": {"allOf": [{"bsonType": "number"}, {"minimum": 2}]}}}},
        FINITE,
    ),
    (
        "schema finite anyOf",
        {"$jsonSchema": {"properties": {"n": {"anyOf": [{"maximum": 0}, {"minimum": 10}]}}}},
        FINITE,
    ),
    (
        "schema finite oneOf",
        {"$jsonSchema": {"properties": {"n": {"oneOf": [{"bsonType": "number"}, {"minimum": 2}]}}}},
        FINITE,
    ),
    (
        "schema finite not minimum",
        {"$jsonSchema": {"properties": {"n": {"not": {"minimum": 5}}}}},
        FINITE,
    ),
    (
        "schema finite ignored on other types",
        {
            "$jsonSchema": {
                "properties": {
                    "n": {"minLength": 9, "minItems": 9, "minimum": -100, "minProperties": 9}
                }
            }
        },
        FINITE,
    ),
    # --- dotted names and paths ----------------------------------------------------------
    ("schema paths properties dotted", _s({"properties": {"a.b": {"bsonType": "int"}}}), PATHS),
    ("schema paths required dotted", _s({"required": ["a.b"]}), PATHS),
    ("schema paths required deep dotted", _s({"required": ["a.b.c"]}), PATHS),
    ("schema paths dependencies dotted", _s({"dependencies": {"a.b": ["_id"]}}), PATHS),
    (
        "schema paths patternProperties dot",
        _s({"patternProperties": {"\\.": {"bsonType": "string"}}}),
        PATHS,
    ),
    (
        "schema paths additionalProperties with dotted property",
        _s({"properties": {"_id": {}, "a.b": {}}, "additionalProperties": False}),
        PATHS,
    ),
    (
        "schema paths properties over array of documents",
        _s({"properties": {"a": {"properties": {"b": {"bsonType": "int"}}}}}),
        PATHS,
    ),
    (
        "schema paths $elemMatch properties",
        {"a": {"$elemMatch": {"$jsonSchema": {"properties": {"b": {"bsonType": "string"}}}}}},
        PATHS,
    ),
    # --- order-insensitive document equality ---------------------------------------------
    (
        "schema uniqueItems nested field order",
        {"$jsonSchema": {"properties": {"u": {"uniqueItems": True}}}},
        [
            {"_id": 1, "u": [[{"a": 1, "b": 2}], [{"b": 2, "a": 1}]]},
            {"_id": 2, "u": [{"x": {"a": 1, "b": 2}}, {"x": {"b": 2, "a": 1}}]},
            {"_id": 3, "u": [{"a": 1, "b": 2}, {"a": 1, "c": 2}]},
        ],
    ),
    (
        "schema enum nested field order",
        _s({"properties": {"a": {"enum": [{"x": {"b": 2, "a": 1}}]}}}),
        [{"_id": 1, "a": {"x": {"a": 1, "b": 2}}}, {"_id": 2, "a": {"x": {"a": 1}}}],
    ),
    (
        "schema enum doc cross-type numbers",
        _s({"properties": {"a": {"enum": [{"b": 1.0}]}}}),
        [{"_id": 1, "a": {"b": 1}}, {"_id": 2, "a": {"b": Decimal128("1")}}],
    ),
    ("schema enum duplicate field order", _a({"enum": [{"a": 1, "b": 2}, {"b": 2, "a": 1}]})),
    ("schema required _id", _s({"required": ["_id"]})),
    # --- encrypt -------------------------------------------------------------------------
    (
        "schema encrypt binData subtype 6",
        _a({"encrypt": {}}),
        [
            {"_id": 1, "a": Binary(b"x", 6)},
            {"_id": 2, "a": Binary(b"x", 0)},
            {"_id": 3, "a": "x"},
            {"_id": 4},
        ],
    ),
    (
        "schema encrypt with bsonType",
        _a({"encrypt": {"bsonType": "string"}}),
        [{"_id": 1, "a": Binary(b"x", 6)}],
    ),
    ("schema encrypt and bsonType together", _a({"encrypt": {}, "bsonType": "binData"})),
    ("schema encrypt not object", _a({"encrypt": 1})),
    # --- nested and combined -------------------------------------------------------------
    (
        "schema deep nesting",
        _s(
            {
                "bsonType": "object",
                "required": ["a"],
                "properties": {
                    "a": {
                        "bsonType": ["object", "array"],
                        "properties": {"b": {"enum": [1, 2]}, "c": {"items": {"minimum": 1}}},
                        "items": {"bsonType": "object", "required": ["b"]},
                    }
                },
            }
        ),
    ),
    (
        "schema nested bad keyword in deep property",
        _s({"properties": {"a": {"properties": {"b": {"items": {"not": {"bogus": 1}}}}}}}),
    ),
    (
        "schema type keywords ignored on other types",
        _a({"minLength": 100, "minItems": 100, "minimum": 100, "minProperties": 100}),
    ),
    (
        "schema everything on missing field",
        _a({"bsonType": "string", "minimum": 100}),
        [{"_id": 1}],
    ),
    ("schema keyword error before unknown", _a({"multipleOf": "x", "bogus": 1})),
    ("schema unknown before keyword error", _a({"bogus": 1, "multipleOf": "x"})),
    # --- translation corners: encrypt's fields, integer arguments, top-level keywords,
    # --- paths through arrays, pattern names (2026-10-22, RM-268 / RM-269) -----------------
    ("schema encrypt unknown field", _a({"encrypt": {"bogus": 1}})),
    ("schema encrypt algorithm unknown", _a({"encrypt": {"algorithm": "x"}})),
    ("schema encrypt algorithm not string", _a({"encrypt": {"algorithm": 1}})),
    (
        "schema encrypt algorithm deterministic",
        _a({"encrypt": {"algorithm": "AEAD_AES_256_CBC_HMAC_SHA_512-Deterministic"}}),
    ),
    ("schema encrypt keyId number", _a({"encrypt": {"keyId": 1}})),
    ("schema encrypt keyId pointer", _a({"encrypt": {"keyId": "/key"}})),
    ("schema encrypt keyId bad pointer", _a({"encrypt": {"keyId": "key"}})),
    ("schema encrypt keyId empty pointer", _a({"encrypt": {"keyId": ""}})),
    ("schema encrypt keyId pointer bare tilde", _a({"encrypt": {"keyId": "/a~2"}})),
    ("schema encrypt keyId pointer trailing tilde", _a({"encrypt": {"keyId": "/a~"}})),
    ("schema encrypt keyId pointer escapes", _a({"encrypt": {"keyId": "/a~0~1/b"}})),
    ("schema encrypt keyId array of strings", _a({"encrypt": {"keyId": ["a"]}})),
    ("schema encrypt keyId array of binData 0", _a({"encrypt": {"keyId": [Binary(b"x" * 16, 0)]}})),
    ("schema encrypt keyId array of uuid", _a({"encrypt": {"keyId": [Binary(b"x" * 16, 4)]}})),
    ("schema encrypt keyId short uuid", _a({"encrypt": {"keyId": [Binary(b"x", 4)]}})),
    ("schema encrypt bsonType unknown", _a({"encrypt": {"bsonType": "bogus"}})),
    ("schema encrypt bsonType number", _a({"encrypt": {"bsonType": 1}})),
    ("schema encrypt bsonType list", _a({"encrypt": {"bsonType": ["string", "int"]}})),
    ("schema encrypt bsonType empty list", _a({"encrypt": {"bsonType": []}})),
    ("schema encrypt and type", _a({"encrypt": {}, "type": "string"})),
    ("schema encrypt and encryptMetadata", _a({"encrypt": {}, "encryptMetadata": {"keyId": "/k"}})),
    ("schema encryptMetadata not object", _s({"encryptMetadata": 1})),
    ("schema encryptMetadata unknown field", _s({"encryptMetadata": {"bsonType": "int"}})),
    ("schema encryptMetadata keyId", _s({"encryptMetadata": {"keyId": "/k"}})),
    ("schema encrypt top level", _s({"encrypt": {}})),
    ("schema minItems nan", _a({"minItems": float("nan")})),
    ("schema maxLength inf", _a({"maxLength": float("inf")})),
    ("schema minProperties decimal nan", _s({"minProperties": Decimal128("NaN")})),
    ("schema minItems decimal fraction", _a({"minItems": Decimal128("2.5")})),
    ("schema minItems negative zero", _a({"minItems": -0.0})),
    ("schema maxItems two to the 63", _a({"maxItems": 9223372036854775808.0})),
    ("schema maxItems long max", _a({"maxItems": Int64(9223372036854775807)})),
    ("schema minItems bool", _a({"minItems": True})),
    ("schema minLength null", _a({"minLength": None})),
    ("schema top-level minimum", _s({"minimum": 5})),
    ("schema top-level minimum string", _s({"minimum": "5"})),
    ("schema top-level multipleOf nan", _s({"multipleOf": float("nan")})),
    ("schema top-level pattern bad", _s({"pattern": "("})),
    ("schema top-level type string", _s({"type": "string"})),
    ("schema top-level type string bad keyword", _s({"type": "string", "minimum": "x"})),
    ("schema top-level bsonType list with object", _s({"bsonType": ["string", "object"]})),
    ("schema top-level enum scalars", _s({"enum": [1, 2]})),
    ("schema top-level enum document", _s({"enum": [1, {"_id": 2, "a": 2.5}, {"a": 1, "_id": 1}]})),
    ("schema top-level items", _s({"items": {"bogus": 1}})),
    ("schema top-level uniqueItems not bool", _s({"uniqueItems": 1})),
    ("schema top-level additionalItems number", _s({"additionalItems": 5})),
    ("schema top-level not", _s({"not": {"required": ["a"]}})),
    ("schema top-level oneOf", _s({"oneOf": [{"required": ["a"]}, {"required": ["b"]}]})),
    (
        "schema dotted minimum through array",
        _s({"properties": {"a.b": {"minimum": 2}}}),
        [
            {"_id": 1, "a": [{"b": 1}, {"b": 3}]},
            {"_id": 2, "a": [{"b": 1}]},
            {"_id": 3, "a": [{"b": "x"}]},
            {"_id": 4, "a": {"b": [1, 3]}},
            {"_id": 5, "a": {"b": [1]}},
            {"_id": 6, "a": [{"b": [5]}, {"c": 1}]},
            {"_id": 7, "a": [1, 2]},
        ],
    ),
    (
        "schema dotted bsonType through array",
        _s({"properties": {"a.b": {"bsonType": "int"}}}),
        [
            {"_id": 1, "a": [{"b": 1}, {"b": "x"}]},
            {"_id": 2, "a": [{"b": "x"}]},
            {"_id": 3, "a": {"b": [1]}},
            {"_id": 4, "a": [{"c": 1}]},
        ],
    ),
    (
        "schema dotted multipleOf and maxLength",
        _s({"properties": {"a.b": {"multipleOf": 2, "maxLength": 1}}}),
        [
            {"_id": 1, "a": [{"b": 3}, {"b": 4}]},
            {"_id": 2, "a": {"b": [3, 4]}},
            {"_id": 3, "a": {"b": ["xy", "z"]}},
            {"_id": 4, "a": [{"b": "xy"}]},
        ],
    ),
    (
        "schema items nested properties",
        _a({"items": {"properties": {"x": {"minimum": 2}}, "required": ["x"]}}),
        [
            {"_id": 1, "a": [{"x": 2}, {"x": 3}]},
            {"_id": 2, "a": [{"x": 2}, {"x": 1}]},
            {"_id": 3, "a": [{"x": 2}, {"y": 1}]},
            {"_id": 4, "a": [{"x": 2}, 5]},
            {"_id": 5, "a": []},
            {"_id": 6, "a": {"x": 1}},
        ],
    ),
    (
        "schema tuple items with additionalItems schema",
        _a(
            {
                "items": [{"bsonType": "int"}, {"bsonType": "string"}],
                "additionalItems": {"minimum": 0},
            }
        ),
        [
            {"_id": 1, "a": [1, "x"]},
            {"_id": 2, "a": [1, "x", 5]},
            {"_id": 3, "a": [1, "x", -5]},
            {"_id": 4, "a": [1]},
            {"_id": 5, "a": ["x", 1]},
            {"_id": 6, "a": [1, "x", "y"]},
        ],
    ),
    (
        "schema additionalItems without tuple",
        _a({"items": {"bsonType": "int"}, "additionalItems": False}),
        [{"_id": 1, "a": [1, 2]}, {"_id": 2, "a": [1, "x"]}],
    ),
    (
        "schema patternProperties unanchored",
        _s({"patternProperties": {"b": {"bsonType": "int"}}}),
        [
            {"_id": 1, "ab": 1, "bc": 2},
            {"_id": 2, "ab": "x"},
            {"_id": 3, "xyz": "x"},
        ],
    ),
    (
        "schema patternProperties and properties both apply",
        _s(
            {
                "properties": {"ab": {"minimum": 5}},
                "patternProperties": {"^a": {"maximum": 6}},
                "additionalProperties": False,
            }
        ),
        [
            {"_id": 1, "ab": 5},
            {"_id": 2, "ab": 7},
            {"_id": 3, "ab": 4},
            {"_id": 4, "ab": 5, "c": 1},
            {"_id": 5, "ac": 1},
        ],
    ),
    (
        "schema nested additionalProperties false",
        _a({"properties": {"x": {}}, "additionalProperties": False}),
        [
            {"_id": 1, "a": {"x": 1}},
            {"_id": 2, "a": {"x": 1, "y": 2}},
            {"_id": 3, "a": [{"x": 1, "y": 2}]},
            {"_id": 4, "a": 5},
        ],
    ),
    (
        "schema nested dependencies",
        _a({"dependencies": {"b": ["c"], "d": {"required": ["e"]}}}),
        [
            {"_id": 1, "a": {"b": 1, "c": 1}},
            {"_id": 2, "a": {"b": 1}},
            {"_id": 3, "a": {"d": 1}},
            {"_id": 4, "a": {"d": 1, "e": 1}},
            {"_id": 5, "a": 5},
            {"_id": 6, "a": [{"b": 1}]},
        ],
    ),
    (
        "schema dependencies title string",
        _s({"dependencies": {"title": "t", "a": ["b"]}}),
    ),
    (
        "schema enum symbol and nan",
        _a({"enum": [float("nan"), "x", -0.0]}),
        [
            {"_id": 1, "a": Decimal128("NaN")},
            {"_id": 2, "a": float("nan")},
            {"_id": 3, "a": 0},
            {"_id": 4, "a": Decimal128("-0")},
            {"_id": 5, "a": "x"},
            {"_id": 6, "a": ["x"]},
            {"_id": 7, "a": Code("x")},
        ],
    ),
    ("schema enum nan duplicate", _a({"enum": [float("nan"), Decimal128("NaN")]})),
    ("schema enum zero duplicate", _a({"enum": [0, -0.0]})),
    ("schema enum array order", _a({"enum": [[1, 2], [2, 1]]})),
    (
        "schema multipleOf decimal divisor and long",
        _a({"multipleOf": Decimal128("0.25")}),
        [
            {"_id": 1, "a": 0.75},
            {"_id": 2, "a": Int64(9007199254740993)},
            {"_id": 3, "a": Decimal128("1E+400")},
            {"_id": 4, "a": 1e300},
            {"_id": 5, "a": 0.1},
            {"_id": 6, "a": Decimal128("-0.5")},
        ],
    ),
    (
        "schema multipleOf tiny double",
        _a({"multipleOf": 1e-300}),
        [{"_id": 1, "a": 1.0}, {"_id": 2, "a": 0.3}, {"_id": 3, "a": 5e-324}],
    ),
    (
        "schema oneOf at path both match",
        _a({"oneOf": [{"minimum": 1}, {"maximum": 10}]}),
        [{"_id": 1, "a": 5}, {"_id": 2, "a": 0}, {"_id": 3, "a": 20}, {"_id": 4, "a": "x"}],
    ),
]

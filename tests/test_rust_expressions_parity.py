"""Parity: Rust `_secantus_core.evaluate` vs pure-Python `expressions.evaluate`.

Phase 1 net for the fourth (largest) leaf engine. For each (expr, doc) the Rust
evaluator runs over BSON bytes; when it returns a concrete result (didn't
defer), the decoded value must equal the authoritative pure-Python `evaluate`.
When it returns None (whole-call fallback — any operator/value not ported)
there's nothing to assert.

Import-light: prefers the real `secantus.expressions`, else loads
`expressions.py` + `paths.py` by path under a stub `secantus` package. The
corpus sticks to the ported core (paths, comparison, logic, control flow,
arithmetic, common array ops) so the pure path never needs the lazily-imported
`secantus.storage` / `bson.Regex` paths.
"""

from __future__ import annotations

import datetime
import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest
from bson import Decimal128, Int64, ObjectId, Timestamp

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_expr():
    try:
        from secantus import expressions as e

        return e
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("paths", "expressions"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.expressions"]


_pure = _load_pure_expr()


def _rust_eval(expr, doc, vars=None):
    res = _rust.evaluate(bson.encode(doc), bson.encode({"e": expr}), bson.encode(vars or {}))
    return None if res is None else bson.decode(res)["r"]


def _bson_norm(v):
    """Normalise a pure-Python result the way BSON storage would — the wire form
    both servers actually return. In particular a tz-aware ``datetime`` collapses
    to its UTC instant (naive), matching the Rust value which is already
    bson-decoded; identity for every other BSON type. This is the faithful
    comparison (stored value), and it can't hide a real value bug — a wrong
    instant still differs after normalisation."""
    return bson.decode(bson.encode({"v": v}))["v"]


_DT = datetime.datetime(2026, 6, 5, 12, 34, 56, tzinfo=datetime.timezone.utc)
_EPOCH = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)


def _mkdate(ms):
    return _EPOCH + datetime.timedelta(milliseconds=ms)


# (expr, doc) pairs over the ported operator core.
CURATED = [
    # $sum/$avg/$max/$min as expression operators (MongoDB 5.0+) — Rust must
    # compute the SAME value + numeric width as Python (int32/int64/double).
    ({"$sum": "$arr"}, {"arr": [1, 2, 3]}),  # int32 result
    ({"$sum": "$arr"}, {"arr": [1, Int64(2), 3]}),  # int64-widened result
    ({"$sum": "$arr"}, {"arr": [1, 2.5, 3]}),  # double result
    ({"$sum": "$n"}, {"n": 5}),  # scalar
    ({"$sum": ["$n", 10, "skip", True]}, {"n": 5}),  # ignore non-numeric + bool
    ({"$sum": "$x"}, {}),  # missing -> 0
    ({"$sum": "$arr"}, {"arr": []}),  # empty -> 0
    ({"$avg": "$arr"}, {"arr": [1, 2, 3]}),  # -> 2.0 (double)
    ({"$avg": "$arr"}, {"arr": [2, 4]}),
    ({"$avg": "$arr"}, {"arr": []}),  # empty -> null
    ({"$avg": "$s"}, {"s": "x"}),  # non-numeric -> null
    ({"$max": "$arr"}, {"arr": [3, 1, 2]}),
    ({"$min": "$arr"}, {"arr": [3, 1, 2]}),
    ({"$max": "$arr"}, {"arr": [1, "a", True]}),  # cross-type BSON order
    ({"$min": "$arr"}, {"arr": [3, None, 1]}),  # null ignored
    ({"$max": "$arr"}, {"arr": []}),  # empty -> null
    ({"$max": "$n"}, {"n": 5}),  # scalar
    # Bitwise ($bitAnd/$bitOr/$bitXor/$bitNot) — int/long, empty-list identity,
    # null propagation, mixed int/long result width. Non-int operands defer.
    ({"$bitAnd": ["$a", "$b"]}, {"a": 12, "b": 10}),
    ({"$bitOr": ["$a", "$b", 1]}, {"a": 12, "b": 10}),
    ({"$bitXor": ["$a", "$b"]}, {"a": 12, "b": 10}),
    ({"$bitNot": "$a"}, {"a": 12}),
    ({"$bitNot": "$a"}, {"a": -5}),
    ({"$bitAnd": ["$a", 255]}, {"a": Int64(0xFF00FF00)}),  # long -> long result
    ({"$bitXor": ["$a", Int64(3)]}, {"a": 12}),  # mixed int/long -> long
    ({"$bitAnd": []}, {}),  # identity -1
    ({"$bitOr": []}, {}),  # identity 0
    ({"$bitAnd": ["$a", "$missing"]}, {"a": 12}),  # null propagation
    ({"$bitAnd": ["$a", 1.5]}, {"a": 12}),  # double operand -> defer (Python raises)
    ({"$bitOr": ["$a", True]}, {"a": 12}),  # bool operand -> defer
    # $firstN / $lastN (expression form) — slice first/last n; n>len -> all.
    # Validation matches mongod: null/missing/non-array input errors (5788200),
    # non-integral / n<=0 errors — all defer (Rust None, Python raises). An
    # integral double n is accepted.
    ({"$firstN": {"n": 2, "input": "$a"}}, {"a": [10, 20, 30, 40]}),
    ({"$lastN": {"n": 2, "input": "$a"}}, {"a": [10, 20, 30, 40]}),
    ({"$firstN": {"n": 10, "input": "$a"}}, {"a": [10, 20]}),
    ({"$lastN": {"n": 10, "input": "$a"}}, {"a": [10, 20]}),
    ({"$firstN": {"n": 1, "input": "$a"}}, {"a": []}),
    ({"$firstN": {"n": 2.0, "input": "$a"}}, {"a": [10, 20, 30]}),  # integral double n
    ({"$firstN": {"n": Int64(2), "input": "$a"}}, {"a": [7, 8, 9]}),  # long n ok
    ({"$firstN": {"n": 2, "input": "$missing"}}, {}),  # missing input -> error (defer)
    ({"$firstN": {"n": 2, "input": None}}, {}),  # null input -> error (defer)
    ({"$firstN": {"n": 0, "input": "$a"}}, {"a": [1, 2]}),  # n<=0 -> defer
    ({"$lastN": {"n": 1.5, "input": "$a"}}, {"a": [1, 2]}),  # non-integral n -> defer
    ({"$firstN": {"n": 2, "input": 5}}, {}),  # non-array -> defer
    # $maxN / $minN (expression form) — n largest/smallest by BSON order; null
    # elements ignored; cross-type sortable subset compares, bool/Decimal128 defers;
    # null/non-array input errors like $firstN (defer).
    ({"$maxN": {"n": 3, "input": "$a"}}, {"a": [3, 1, 4, 1, 5, 9, 2, 6]}),
    ({"$minN": {"n": 3, "input": "$a"}}, {"a": [3, 1, 4, 1, 5, 9, 2, 6]}),
    ({"$maxN": {"n": 2, "input": "$a"}}, {"a": [3, None, 1, None, 5]}),  # nulls ignored
    ({"$minN": {"n": 2, "input": "$a"}}, {"a": [3, None, 1, None, 5]}),
    ({"$maxN": {"n": 99, "input": "$a"}}, {"a": [3, 1, 2]}),  # n>len -> all sorted
    ({"$minN": {"n": 2, "input": [None, None]}}, {}),  # all-null array -> []
    ({"$maxN": {"n": 2, "input": "$missing"}}, {}),  # missing input -> error (defer)
    ({"$maxN": {"n": 2, "input": ["b", "a", "c"]}}, {}),  # strings
    ({"$maxN": {"n": 2, "input": [1, "a", 2.5, "b"]}}, {}),  # cross-type BSON order
    ({"$maxN": {"n": 1, "input": [True, 1]}}, {}),  # bool element -> defer
    ({"$maxN": {"n": 0, "input": "$a"}}, {"a": [1, 2]}),  # n<=0 -> defer
    ({"$minN": {"n": 2, "input": 5}}, {}),  # non-array -> defer
    ("$a", {"a": 5}),
    ("hi", {}),
    ("$missing", {}),
    ("$a.b", {"a": {"b": 7}}),
    ({"$literal": "$notapath"}, {}),
    ({"$eq": ["$a", "$b"]}, {"a": 5, "b": 5}),
    ({"$eq": [1, 1.0]}, {}),
    ({"$eq": [True, 1]}, {}),
    ({"$ne": [1, 2]}, {}),
    ({"$gt": ["$a", "$b"]}, {"a": 5, "b": 3}),
    ({"$gt": ["$a", "$b"]}, {"a": 1, "b": 3}),
    ({"$gte": [5, 5]}, {}),
    ({"$lt": ["apple", "banana"]}, {}),
    ({"$lte": [3, 2]}, {}),
    ({"$gt": [1, "x"]}, {}),  # cross-type -> False
    ({"$and": [True, {"$eq": [1, 1]}]}, {}),
    ({"$or": [False, {"$gt": [2, 5]}]}, {}),
    ({"$not": [{"$eq": [1, 2]}]}, {}),
    ({"$cond": [{"$gt": ["$a", 0]}, "pos", "neg"]}, {"a": 5}),
    ({"$cond": {"if": {"$gt": ["$a", 0]}, "then": "pos", "else": "neg"}}, {"a": -1}),
    ({"$ifNull": ["$x", 7]}, {}),
    ({"$ifNull": ["$a", 7]}, {"a": 3}),
    (
        {
            "$switch": {
                "branches": [{"case": {"$gt": ["$a", 10]}, "then": "big"}],
                "default": "small",
            }
        },
        {"a": 5},
    ),
    ({"$add": [1, 2, 3]}, {}),
    ({"$add": ["$a", 1]}, {}),  # missing -> null
    ({"$add": [1.5, 2]}, {}),
    ({"$add": [5]}, {}),
    ({"$subtract": [10, 3]}, {}),
    ({"$multiply": [2, 3, 4]}, {}),
    ({"$multiply": [2, 2.5]}, {}),
    ({"$divide": [9, 2]}, {}),
    ({"$divide": [6, 0]}, {}),  # /0 -> null
    ({"$mod": [7, 3]}, {}),
    ({"$mod": [-7, 3]}, {}),
    ({"$size": "$a"}, {"a": [1, 2, 3]}),
    ({"$arrayElemAt": ["$a", -1]}, {"a": [10, 20, 30]}),
    ({"$first": "$a"}, {"a": [1, 2]}),
    ({"$last": "$a"}, {"a": [1, 2]}),
    ({"$concatArrays": [[1, 2], [3]]}, {}),
    ({"$reverseArray": [1, 2, 3]}, {}),
    # Non-array input: Rust defers (None), Python raises the per-op code; null /
    # missing input yields null on both.
    ({"$first": 5}, {}),
    ({"$last": "$a"}, {"a": "x"}),
    ({"$first": "$x"}, {}),  # missing -> null
    ({"$reverseArray": 5}, {}),
    ({"$reverseArray": None}, {}),  # null -> null
    ({"$concatArrays": [[1], 5]}, {}),
    ({"$concatArrays": [[1], None]}, {}),  # null operand -> null
    ({"$map": {"input": 5, "in": "$$this"}}, {}),
    ({"$map": {"input": None, "in": "$$this"}}, {}),  # null -> null
    ({"$filter": {"input": 5, "cond": True}}, {}),
    ({"$reduce": {"input": 5, "initialValue": 0, "in": "$$value"}}, {}),
    # $sortArray. Int form: homogeneous / numeric scalars (Python's native sort
    # raises on docs or incomparable mixes, so the corpus avoids those). Doc form
    # uses BSON sort order on the named fields.
    ({"$sortArray": {"input": [3, 1, 2], "sortBy": 1}}, {}),
    ({"$sortArray": {"input": [3, 1, 2], "sortBy": -1}}, {}),
    ({"$sortArray": {"input": [3, 1.5, 2, 1.5], "sortBy": 1}}, {}),  # numeric + stable ties
    ({"$sortArray": {"input": ["b", "a", "c"], "sortBy": -1}}, {}),
    ({"$sortArray": {"input": "$xs", "sortBy": 1}}, {"xs": [5, 2, 8, 1]}),
    ({"$sortArray": {"input": "$missing", "sortBy": 1}}, {}),  # missing input -> null
    ({"$sortArray": {"input": [{"x": 3}, {"x": 1}, {"x": 2}], "sortBy": {"x": 1}}}, {}),
    (
        {
            "$sortArray": {
                "input": [{"a": 1, "b": 2}, {"a": 1, "b": 1}, {"a": 0, "b": 9}],
                "sortBy": {"a": 1, "b": -1},
            }
        },
        {},
    ),
    # missing sort field sorts as null (first)
    ({"$sortArray": {"input": [{"x": 2}, {"y": 1}], "sortBy": {"x": 1}}}, {}),
    ({"$in": [2, [1, 2, 3]]}, {}),
    ({"$in": [9, [1, 2, 3]]}, {}),
    # Nested
    ({"$cond": [{"$lt": [{"$add": ["$p", "$q"]}, 100]}, "cheap", "dear"]}, {"p": 30, "q": 40}),
    # String / array / object ops (now handled).
    ({"$concat": ["a", "b", "c"]}, {}),
    ({"$concat": ["x", "$a", "y"]}, {"a": "MID"}),
    ({"$concat": ["a", None, "b"]}, {}),  # null operand -> null result
    ({"$concat": ["a", "$nope", "b"]}, {}),  # missing -> null
    # Non-string operand: Rust defers (None); Python raises 16702.
    ({"$concat": ["a", 5]}, {}),
    ({"$concat": ["a", True]}, {}),
    ({"$concat": [5, None]}, {}),  # left-to-right: non-string before null -> raise
    ({"$toUpper": "$a"}, {"a": "hi"}),
    ({"$toLower": "HELLO"}, {}),
    ({"$toUpper": "$a"}, {"a": 123}),  # non-string passes through
    ({"$strLenCP": "hello"}, {}),
    ({"$split": ["a,b,c", ","]}, {}),
    ({"$split": ["$a", "-"]}, {"a": "1-2-3"}),
    # Invalid $split: Rust defers (None); Python raises 40085/40086/40087/16020.
    ({"$split": ["a,b", ""]}, {}),  # empty sep
    ({"$split": [5, ","]}, {}),  # non-string first
    ({"$split": ["a,b", 5]}, {}),  # non-string second
    ({"$split": ["a,b"]}, {}),  # wrong arg count
    ({"$split": [None, ","]}, {}),  # null string -> null (both compute)
    ({"$split": ["a,b", None]}, {}),  # null sep -> null
    ({"$substrCP": ["hello", 1, 3]}, {}),
    ({"$substrCP": ["hello", 2, -1]}, {}),  # negative length -> to end
    ({"$substrCP": ["hello", 10, 2]}, {}),  # start past end -> ""
    ({"$slice": [[1, 2, 3, 4], 2]}, {}),
    ({"$slice": [[1, 2, 3, 4], -2]}, {}),
    ({"$slice": [[1, 2, 3, 4, 5], 1, 2]}, {}),
    ({"$slice": [[1, 2, 3, 4, 5], -3, 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3, 2], 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3, 2], 2, 2]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 9]}, {}),
    ({"$mergeObjects": [{"a": 1}, {"b": 2}, {"a": 9}]}, {}),
    ({"$mergeObjects": [{"a": 1}, None, {"b": 2}]}, {}),
    ({"$objectToArray": "$o"}, {"o": {"x": 1, "y": 2}}),
    # Scope-introducing ops.
    ({"$map": {"input": [1, 2, 3], "in": {"$add": ["$$this", 10]}}}, {}),
    ({"$map": {"input": "$xs", "as": "n", "in": {"$multiply": ["$$n", 2]}}}, {"xs": [1, 2, 3]}),
    ({"$filter": {"input": [1, 2, 3, 4], "as": "n", "cond": {"$gt": ["$$n", 2]}}}, {}),
    ({"$filter": {"input": [1, 2, 3, 4, 5], "cond": {"$lt": ["$$this", 5]}, "limit": 2}}, {}),
    (
        {
            "$reduce": {
                "input": [1, 2, 3, 4],
                "initialValue": 0,
                "in": {"$add": ["$$value", "$$this"]},
            }
        },
        {},
    ),
    (
        {"$reduce": {"input": "$xs", "initialValue": "", "in": {"$concat": ["$$value", "$$this"]}}},
        {"xs": ["a", "b", "c"]},
    ),
    ({"$let": {"vars": {"d": {"$add": ["$x", 1]}}, "in": {"$multiply": ["$$d", 2]}}}, {"x": 5}),
    # Set operators — union/intersection BSON-sorted, difference first-array order;
    # dedup by BSON equality; a non-array arg or unsortable element defers.
    ({"$setUnion": [[3, 1, 2], [5, 4]]}, {}),
    ({"$setUnion": [[3, 3, 1], [1, 2]]}, {}),
    ({"$setUnion": [["b", 1, "a"], [2, "a"]]}, {}),  # cross-type order
    ({"$setUnion": [[1], [1.0]]}, {}),  # 1 == 1.0 dedup
    ({"$setIntersection": [[3, 1, 2, 5], [2, 5, 1]]}, {}),
    ({"$setIntersection": [[1, 2], [3, 4]]}, {}),  # empty
    ({"$setDifference": [[5, 3, 1, 2], [3]]}, {}),
    ({"$setEquals": [[1, 2], [2, 1]]}, {}),
    ({"$setEquals": [[1, 2], [1, 3]]}, {}),
    ({"$setIsSubset": [[1, 2], [1, 2, 3]]}, {}),
    ({"$setIsSubset": [[1, 4], [1, 2, 3]]}, {}),
    ({"$allElementsTrue": [[1, True]]}, {}),
    ({"$allElementsTrue": [[1, 0]]}, {}),
    ({"$anyElementTrue": [[0, False, 1]]}, {}),
    ({"$setUnion": [[1], 5]}, {}),  # non-array -> defer
    ({"$setUnion": [[True, 1]]}, {}),  # bool element -> defer (unsortable)
    # $cmp / $binarySize / $bsonSize / degrees.
    ({"$cmp": [1, 2]}, {}),
    ({"$cmp": [5, 5]}, {}),
    ({"$cmp": ["b", "a"]}, {}),
    ({"$cmp": [1, "x"]}, {}),  # cross-type BSON order
    ({"$binarySize": "$s"}, {"s": "héllo"}),
    ({"$binarySize": "$x"}, {}),  # missing -> null
    ({"$bsonSize": "$$ROOT"}, {"a": 5, "b": "x"}),
    ({"$degreesToRadians": 180}, {}),
    ({"$degreesToRadians": 45}, {}),
    ({"$radiansToDegrees": 1}, {}),
    ({"$radiansToDegrees": "$x"}, {}),  # missing -> null
    # Trig family (libm: Rust f64 and CPython math share the platform libm, so
    # they agree bit-for-bit; Decimal128 / bool / domain violations defer).
    ({"$sin": 0}, {}),
    ({"$sin": 1}, {}),
    ({"$sin": 0.5}, {}),
    ({"$cos": 1}, {}),
    ({"$tan": 0.5}, {}),
    ({"$asin": 0.5}, {}),
    ({"$asin": 1}, {}),
    ({"$acos": -0.5}, {}),
    ({"$atan": 3.14159}, {}),
    ({"$atan2": [1, 1]}, {}),
    ({"$atan2": [1, 0]}, {}),
    ({"$atan2": [0, 0]}, {}),
    ({"$sinh": 1}, {}),
    ({"$cosh": 0.5}, {}),
    ({"$tanh": 2}, {}),
    ({"$asinh": 1}, {}),
    ({"$acosh": 2}, {}),
    ({"$acosh": 1}, {}),
    ({"$atanh": 0.5}, {}),
    ({"$atanh": -0.5}, {}),  # negative: Rust f64::atanh is off by 1 ULP -> forced odd symmetry
    ({"$atanh": -0.9}, {}),
    ({"$atanh": 1}, {}),  # -> inf
    ({"$atanh": -1}, {}),  # -> -inf
    ({"$sin": None}, {}),  # null -> null
    ({"$sin": "$x"}, {}),  # missing -> null
    ({"$asin": 5}, {}),  # out of [-1,1] -> defer
    ({"$acosh": 0.5}, {}),  # < 1 -> defer
    ({"$cos": "hi"}, {}),  # non-numeric -> defer
    ({"$sin": True}, {}),  # bool -> defer
    ({"$atan2": ["hi", 1]}, {}),  # atan2 non-numeric -> defer
    # $map referencing a ROOT field path inside `in` ($$CURRENT stays ROOT).
    ({"$map": {"input": [1, 2], "in": {"$add": ["$$this", "$base"]}}}, {"base": 100}),
    # Date component extractors.
    ({"$year": "$d"}, {"d": _DT}),
    ({"$month": "$d"}, {"d": _DT}),
    ({"$dayOfMonth": "$d"}, {"d": _DT}),
    ({"$hour": "$d"}, {"d": _DT}),
    ({"$dayOfWeek": "$d"}, {"d": _DT}),
    ({"$year": "$d"}, {"d": "not a date"}),  # non-date -> null
    # Date extractors — {date, timezone} object form. Instant->wall-clock is
    # unambiguous, so fixed-offset and named IANA zones (via chrono-tz) both match
    # Python zoneinfo. Corpus curated to post-2007 dates in decade-stable zones.
    ({"$hour": {"date": "$d"}}, {"d": _DT}),  # no tz -> UTC
    ({"$hour": {"date": "$d", "timezone": "+05:30"}}, {"d": _DT}),  # fixed offset
    (
        {"$hour": {"date": "$d", "timezone": "America/New_York"}},
        {"d": datetime.datetime(2023, 1, 15, 16, 30, tzinfo=datetime.timezone.utc)},  # winter EST
    ),
    (
        {"$dayOfMonth": {"date": "$d", "timezone": "America/New_York"}},
        # 02:30Z on the 15th is 21:30 EST on the 14th — the tz shift crosses midnight.
        {"d": datetime.datetime(2023, 1, 15, 2, 30, tzinfo=datetime.timezone.utc)},
    ),
    (
        {"$hour": {"date": "$d", "timezone": "Asia/Tokyo"}},
        {"d": _DT},  # JST +09:00
    ),
    (
        {"$dayOfWeek": {"date": "$d", "timezone": "Australia/Sydney"}},
        {"d": _DT},
    ),
    ({"$year": {"date": "$d", "timezone": "America/New_York"}}, {"d": _DT}),
    ({"$hour": {"date": "$x", "timezone": "UTC"}}, {}),  # missing date -> null
    ({"$hour": {"date": "$d", "timezone": "Not/AZone"}}, {"d": _DT}),  # unknown -> defer
    # Type conversions (safe subset).
    ({"$toInt": 3.9}, {}),
    ({"$toInt": "$n"}, {"n": -3.9}),
    ({"$toInt": True}, {}),
    ({"$toInt": "$n"}, {"n": Int64(5)}),  # int64 -> int32 (mongod always yields int)
    ({"$toInt": "$x"}, {}),  # missing -> null
    ({"$toInt": 3e9}, {}),  # > int32 max -> overflow (both defer)
    ({"$toInt": "$n"}, {"n": Int64(2**40)}),  # int64 > int32 -> overflow
    ({"$toInt": 2147483647.0}, {}),  # int32 max exactly -> ok
    ({"$toInt": float("inf")}, {}),  # non-finite -> overflow (both defer)
    # $toLong — int64 target: computes numeric cases, defers string/Decimal128/overflow.
    ({"$toLong": 3.9}, {}),
    ({"$toLong": "$n"}, {"n": -3.9}),
    ({"$toLong": True}, {}),
    ({"$toLong": 9_000_000_000.0}, {}),  # > int32, within int64 -> ok
    ({"$toLong": "$n"}, {"n": Int64(2**40)}),  # int64 passes through
    ({"$toLong": "$x"}, {}),  # missing -> null
    ({"$toLong": 1e30}, {}),  # > int64 max -> overflow (both defer)
    ({"$toLong": float("inf")}, {}),  # non-finite -> defer
    ({"$toLong": "42"}, {}),  # string parse -> defer
    ({"$toDouble": 5}, {}),
    ({"$toDouble": False}, {}),
    ({"$toDouble": "$n"}, {"n": Int64(7)}),
    ({"$toBool": 0}, {}),
    ({"$toBool": 5}, {}),
    ({"$toBool": ""}, {}),
    ({"$toBool": "x"}, {}),
    ({"$toBool": "$d"}, {"d": _DT}),  # datetime -> True
    ({"$toString": 42}, {}),
    ({"$toString": "$n"}, {"n": Int64(-9)}),
    ({"$toString": True}, {}),
    ({"$toString": "$s"}, {"s": "hi"}),
    ({"$toInt": "12"}, {}),  # string parse -> defer
    ({"$toString": 3.14}, {}),  # float str() -> defer
    # $toDecimal: int / bool / finite double (shortest round-trip text) / numeric
    # string / Decimal128 passthrough. Floats use exactly-representable values so
    # `{:?}` text matches Python's `repr` and the Decimal128 bytes agree.
    ({"$toDecimal": 5}, {}),
    ({"$toDecimal": "$n"}, {"n": Int64(7)}),
    ({"$toDecimal": True}, {}),
    ({"$toDecimal": 4.125}, {}),
    ({"$toDecimal": "1234.5678"}, {}),
    ({"$toDecimal": "$d"}, {"d": Decimal128("3.14")}),  # passthrough
    ({"$toDecimal": "$x"}, {}),  # missing -> null
    ({"$toDecimal": "notanumber"}, {}),  # unparseable -> defer
    # $convert: bounded port (numeric / bool / decimal targets + null/onNull/
    # onError). String / objectId targets and string/Decimal128 numeric sources
    # defer to Python. Targets given as both alias strings and numeric codes.
    ({"$convert": {"input": 5, "to": "double"}}, {}),
    ({"$convert": {"input": True, "to": 1}}, {}),  # numeric target code
    ({"$convert": {"input": 5, "to": "bool"}}, {}),
    ({"$convert": {"input": 0, "to": "bool"}}, {}),
    ({"$convert": {"input": "", "to": "bool"}}, {}),
    ({"$convert": {"input": "hi", "to": "bool"}}, {}),
    ({"$convert": {"input": True, "to": "int"}}, {}),
    ({"$convert": {"input": 7.9, "to": "int"}}, {}),  # truncates
    ({"$convert": {"input": 5, "to": "long"}}, {}),  # -> Int64
    ({"$convert": {"input": 3e9, "to": "int"}}, {}),  # > int32 -> overflow (defer)
    ({"$convert": {"input": 9.3e18, "to": "long"}}, {}),  # > int64 -> overflow (defer)
    ({"$convert": {"input": 1e30, "to": "int", "onError": "oops"}}, {}),  # overflow -> onError
    ({"$convert": {"input": "1234.5678", "to": "decimal"}}, {}),
    ({"$convert": {"input": 5, "to": "decimal"}}, {}),
    ({"$convert": {"input": 4.125, "to": "decimal"}}, {}),
    ({"$convert": {"input": "$dt", "to": "date"}}, {"dt": datetime.datetime(2021, 1, 2)}),
    ({"$convert": {"input": "$x", "to": "int", "onNull": -1}}, {}),  # null -> onNull
    ({"$convert": {"input": "$x", "to": "int"}}, {}),  # null -> null
    ({"$convert": {"input": "nope", "to": "decimal", "onError": -7}}, {}),  # fail -> onError
    # $convert defers (Rust None; Python not invoked since rust is None first).
    ({"$convert": {"input": 5, "to": "string"}}, {}),  # string target -> defer
    ({"$convert": {"input": "notanumber", "to": "int"}}, {}),  # str->int defer
    ({"$convert": {"input": "$d", "to": "int"}}, {"d": Decimal128("3.5")}),  # dec->int defer
    ({"$convert": {"input": 5}}, {}),  # missing `to` -> defer
    # $toDate: shorthand for $convert to "date". Rust handles the date
    # passthrough natively (asserts) and defers int-millis / string to Python
    # exactly like the $convert date path (keeping the tz-aware / naive datetime
    # parity intact). null / missing -> null (handled in Rust). An ObjectId is
    # unsupported by the $convert date path, so $toDate defers it too (Rust None).
    ({"$toDate": "$d"}, {"d": _DT}),  # date passthrough -> asserts
    ({"$toDate": "$x"}, {}),  # missing -> null
    ({"$toDate": None}, {}),  # null -> null
    ({"$toDate": 1700000000000}, {}),  # int millis -> defer to Python
    ({"$toDate": "$n"}, {"n": Int64(1700000000000)}),  # int64 millis -> defer
    ({"$toDate": "$s"}, {"s": "2026-04-28T12:00:00"}),  # ISO string -> defer
    ({"$toDate": "$o"}, {"o": ObjectId("507f1f77bcf86cd799439011")}),  # objectId -> defer
    ({"$toDate": True}, {}),  # bool -> defer (Python raises 241)
    # $regexMatch / $regexFind / $regexFindAll — ASCII patterns (byte offset ==
    # code-point idx), simple captures. The linear `regex` crate's leftmost-first
    # semantics align with Python `re` here.
    ({"$regexMatch": {"input": "hello world", "regex": "wor"}}, {}),
    ({"$regexMatch": {"input": "hello", "regex": "^h"}}, {}),
    ({"$regexMatch": {"input": "Hello", "regex": "hello", "options": "i"}}, {}),
    ({"$regexMatch": {"input": "hello", "regex": "xyz"}}, {}),  # no match -> False
    ({"$regexMatch": {"input": 123, "regex": "1"}}, {}),  # non-string input -> False
    ({"$regexMatch": {"input": "$s", "regex": "b"}}, {"s": "abc"}),  # field-path input
    ({"$regexMatch": {"input": "aa", "regex": r"(a)\1"}}, {}),  # backref -> fancy is_match
    ({"$regexFind": {"input": "hello world", "regex": "o"}}, {}),
    ({"$regexFind": {"input": "2024-01", "regex": r"(\d+)-(\d+)"}}, {}),  # captures
    ({"$regexFind": {"input": "a", "regex": "(a)(b)?"}}, {}),  # non-participating -> null
    ({"$regexFind": {"input": "abc", "regex": "x"}}, {}),  # no match -> null
    ({"$regexFind": {"input": 5, "regex": "5"}}, {}),  # non-string input -> null
    ({"$regexFindAll": {"input": "a1b2c3", "regex": r"\d"}}, {}),
    ({"$regexFindAll": {"input": "a1b2", "regex": r"([a-z])(\d)"}}, {}),  # captures
    ({"$regexFindAll": {"input": "xyz", "regex": r"\d"}}, {}),  # none -> []
    ({"$regexFindAll": {"input": 5, "regex": "."}}, {}),  # non-string -> []
    # Fancy-regex finds: backreferences / lookaround now compute (the backtracking
    # engine is Python-`re`-compatible), no longer deferring.
    ({"$regexFind": {"input": "hello", "regex": r"(l)\1"}}, {}),  # backref -> "ll" @2
    ({"$regexFind": {"input": "foobar", "regex": r"foo(?=bar)"}}, {}),  # lookahead, no capture
    ({"$regexFind": {"input": "xfoobar", "regex": r"(?<=x)foo"}}, {}),  # lookbehind @1
    ({"$regexFindAll": {"input": "aabbcc", "regex": r"(.)\1"}}, {}),  # backref, all pairs
    ({"$regexMatch": {"input": "foobar", "regex": r"foo(?=bar)"}}, {}),  # lookahead match
    # Math (deterministic subset).
    ({"$abs": -5}, {}),
    ({"$abs": "$n"}, {"n": -5.5}),
    ({"$abs": -(2**31)}, {}),  # -> int64
    ({"$abs": "$x"}, {}),  # missing -> null
    # Non-numeric operands defer (Rust None; Python raises 28765 / 51081). If the
    # Rust core ever computed one, the harness would evaluate Python and surface
    # the raise — so these pin "reject, don't coerce" on both engines.
    ({"$abs": "$s"}, {"s": "x"}),  # string -> defer
    ({"$abs": True}, {}),  # bool -> defer (no coercion to 1)
    ({"$ceil": True}, {}),
    ({"$floor": "$s"}, {"s": "x"}),
    ({"$sqrt": True}, {}),
    ({"$exp": True}, {}),
    ({"$ln": "$s"}, {"s": "x"}),
    ({"$log10": True}, {}),
    ({"$trunc": True}, {}),
    ({"$trunc": "$s"}, {"s": "x"}),
    ({"$round": True}, {}),
    ({"$floor": 3.7}, {}),
    ({"$floor": -3.2}, {}),
    ({"$floor": 5}, {}),
    ({"$ceil": 3.2}, {}),
    ({"$ceil": -3.7}, {}),
    ({"$sqrt": 16}, {}),
    ({"$sqrt": 2}, {}),
    ({"$sqrt": "$n"}, {"n": -1}),  # negative -> defers (Python raises 28714)
    ({"$sqrt": 0}, {}),
    # $exp / $ln / $log (libm: Rust f64 and CPython math share the platform libm,
    # so the bits agree; the test asserts rust == py, not a literal).
    ({"$exp": 0}, {}),
    ({"$exp": 1}, {}),
    ({"$exp": "$x"}, {}),  # missing -> null
    ({"$ln": 1}, {}),
    ({"$ln": "$n"}, {"n": 2.5}),
    ({"$ln": 0}, {}),  # <= 0 -> defers (Python raises 28766)
    ({"$ln": -3}, {}),  # -> defers
    ({"$log": [8, 2]}, {}),
    ({"$log": [100, 10]}, {}),
    ({"$log": [8, 1]}, {}),  # base 1 -> defers (Python raises 28759)
    ({"$log": [None, 2]}, {}),  # null arg -> null
    ({"$log": ["$s", 2]}, {"s": "x"}),  # non-numeric arg -> defer (28756)
    ({"$log": [8, "$s"]}, {"s": "x"}),  # non-numeric base -> defer (28757)
    ({"$log": [True, 2]}, {}),  # bool arg -> defer (no coercion)
    ({"$log": [8, True]}, {}),  # bool base -> defer
    ({"$log10": 100}, {}),
    ({"$log10": 1000}, {}),
    ({"$log10": "$n"}, {"n": 2.5}),
    ({"$log10": 0}, {}),  # <= 0 -> defers (Python raises 28761)
    ({"$log10": -5}, {}),  # -> defers
    ({"$log10": "$missing"}, {}),  # missing -> null
    # $pow: int**non-neg-int -> int; float operand / negative exp -> double.
    ({"$pow": [2, 10]}, {}),
    ({"$pow": [2, 0]}, {}),
    ({"$pow": [2, -1]}, {}),  # -> 0.5 (double)
    ({"$pow": [2.0, 3]}, {}),  # float base -> double
    ({"$pow": ["$b", 3]}, {"b": 3}),
    # $round: half-to-even; int stays int, double rounds to `place` decimals.
    ({"$round": [3.14159, 2]}, {}),
    ({"$round": [2.25, 1]}, {}),  # banker's: 2.25 -> 2.2
    ({"$round": 5}, {}),  # int -> unchanged int
    ({"$round": [15, -1]}, {}),  # -> 20 (int)
    ({"$round": "$x"}, {}),  # missing -> null
    # $trunc: truncate toward zero; always a double (Python `/` float division).
    ({"$trunc": [3.789, 2]}, {}),
    ({"$trunc": 5}, {}),  # -> 5.0 (double)
    ({"$trunc": [-3.789, 1]}, {}),
    # $dateToParts — UTC and timezone (instant->wall-clock; fixed-offset + named
    # IANA zones both compute, curated to post-2007 dates in decade-stable zones).
    ({"$dateToParts": {"date": "$d"}}, {"d": _DT}),
    ({"$dateToParts": {"date": "$x"}}, {}),  # missing -> null
    ({"$dateToParts": {"date": "$d", "timezone": "+05:30"}}, {"d": _DT}),
    (
        {"$dateToParts": {"date": "$d", "timezone": "America/New_York"}},
        {"d": datetime.datetime(2023, 1, 15, 16, 30, 45, tzinfo=datetime.timezone.utc)},
    ),
    ({"$dateToParts": {"date": "$d", "timezone": "Asia/Tokyo"}}, {"d": _DT}),
    ({"$dateToParts": {"date": "$d", "timezone": "Not/AZone"}}, {"d": _DT}),  # unknown -> defer
    # $dateFromParts — calendar build with rollover; defaults month/day=1, time=0;
    # any null component -> null; fixed-offset tz is local->instant. Non-integral,
    # missing/out-of-range year, ISO-week form, named tz all defer (Python raises
    # or computes via zoneinfo).
    ({"$dateFromParts": {"year": 2023, "month": 6, "day": 15}}, {}),
    ({"$dateFromParts": {"year": 2023, "month": 13, "day": 1}}, {}),  # rollover
    ({"$dateFromParts": {"year": 2023, "month": 0, "day": 1}}, {}),
    ({"$dateFromParts": {"year": 2023, "month": 6, "day": 0}}, {}),
    ({"$dateFromParts": {"year": 2023, "month": 6, "day": 15, "hour": 25}}, {}),
    ({"$dateFromParts": {"year": 2023, "month": -1}}, {}),
    ({"$dateFromParts": {"year": 2023.0, "month": 6.0, "day": 15.0}}, {}),  # integral doubles
    ({"$dateFromParts": {"year": 2023, "millisecond": 1500}}, {}),
    ({"$dateFromParts": {"year": "$y", "month": 3, "day": 15}}, {"y": 2024}),  # expr year
    (
        {"$dateFromParts": {"year": 2023, "month": 6, "day": 15, "hour": 12, "timezone": "+05:00"}},
        {},
    ),
    ({"$dateFromParts": {"year": 2023, "timezone": "-08:00"}}, {}),
    ({"$dateFromParts": {"year": None, "month": 6}}, {}),  # null -> null
    ({"$dateFromParts": {"year": 2023, "month": 6.5}}, {}),  # non-integral -> defer
    ({"$dateFromParts": {"month": 6}}, {}),  # missing year -> defer
    ({"$dateFromParts": {"year": 10000}}, {}),  # year range -> defer
    ({"$dateFromParts": {"isoWeekYear": 2023}}, {}),  # iso form -> defer
    ({"$dateFromParts": {"year": 2023, "timezone": "America/New_York"}}, {}),  # named tz -> defer
    # $dateFromParts ISO-week form — Monday of ISO week 1 + (week-1)/(day-1) rollover.
    ({"$dateFromParts": {"isoWeekYear": 2023, "isoWeek": 5, "isoDayOfWeek": 3}}, {}),
    ({"$dateFromParts": {"isoWeekYear": 2023}}, {}),  # defaults isoWeek/day = 1
    ({"$dateFromParts": {"isoWeekYear": 2023, "isoWeek": 53, "isoDayOfWeek": 1}}, {}),  # rollover
    ({"$dateFromParts": {"isoWeekYear": 2023, "isoWeek": 5, "timezone": "+05:00"}}, {}),
    ({"$dateFromParts": {"isoWeek": 5}}, {}),  # missing isoWeekYear -> defer
    # $tsSecond / $tsIncrement — Timestamp fields; null -> null; non-ts -> defer.
    ({"$tsSecond": "$t"}, {"t": Timestamp(1700000000, 7)}),
    ({"$tsIncrement": "$t"}, {"t": Timestamp(1700000000, 7)}),
    ({"$tsSecond": "$x"}, {}),  # missing -> null
    ({"$tsSecond": 5}, {}),  # non-timestamp -> defer
    # $type — full type coverage incl. missing.
    ({"$type": "$a"}, {"a": 5}),
    ({"$type": "$a"}, {"a": Int64(9)}),
    ({"$type": "$a"}, {"a": 3.5}),
    ({"$type": "$a"}, {"a": Decimal128("1.5")}),
    ({"$type": "$a"}, {"a": True}),
    ({"$type": "$a"}, {"a": "s"}),
    ({"$type": "$a"}, {"a": [1]}),
    ({"$type": "$a"}, {"a": None}),  # explicit null
    ({"$type": "$a"}, {"a": ObjectId()}),
    ({"$type": "$a"}, {"a": Timestamp(1, 2)}),
    ({"$type": "$missing"}, {}),  # missing field -> "missing"
    ({"$type": {"$literal": 5}}, {}),
    # $isNumber / $isArray.
    ({"$isNumber": "$a"}, {"a": 5}),
    ({"$isNumber": "$a"}, {"a": Decimal128("1.5")}),
    ({"$isNumber": "$a"}, {"a": True}),
    ({"$isNumber": "$a"}, {"a": "s"}),
    ({"$isNumber": "$x"}, {}),  # missing -> False
    ({"$isArray": "$a"}, {"a": [1, 2]}),
    ({"$isArray": "$a"}, {"a": "s"}),
    # $strcasecmp — case-insensitive; null -> "".
    ({"$strcasecmp": ["abc", "ABC"]}, {}),
    ({"$strcasecmp": ["a", "b"]}, {}),
    ({"$strcasecmp": ["b", "a"]}, {}),
    ({"$strcasecmp": ["$n", "a"]}, {"n": None}),
    ({"$strcasecmp": ["café", "CAFÉ"]}, {}),  # non-ASCII -> defer
    # mongod $toString-coerces operands: an int matches Python str(int) on both
    # engines; null -> "". (double/date/bool coercion defers to Python.)
    ({"$strcasecmp": [5, "a"]}, {}),
    ({"$strcasecmp": ["a", 5]}, {}),
    ({"$strcasecmp": [5, 10]}, {}),
    ({"$strcasecmp": ["$n", "a"]}, {"n": 42}),
    # $replaceOne / $replaceAll.
    ({"$replaceOne": {"input": "abcabc", "find": "bc", "replacement": "X"}}, {}),
    ({"$replaceAll": {"input": "abcabc", "find": "bc", "replacement": "X"}}, {}),
    ({"$replaceOne": {"input": "xyz", "find": "a", "replacement": "b"}}, {}),  # no match
    (
        {"$replaceAll": {"input": "$n", "find": "a", "replacement": "b"}},
        {"n": None},
    ),  # null -> null
    ({"$replaceOne": {"input": "abc", "find": 5, "replacement": "b"}}, {}),  # non-string -> defer
    # $dateFromString — parity-safe slice: naive canonical ISO (date-only /
    # whole-second), no format/timezone. Fractional / Z / offset / format /
    # timezone / invalid all defer.
    ({"$dateFromString": {"dateString": "2024-01-15"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00"}}, {}),
    ({"$dateFromString": {"dateString": "$s"}}, {"s": "2020-02-29T23:59:59"}),  # leap day
    ({"$dateFromString": {"dateString": None}}, {}),  # null -> null
    ({"$dateFromString": {"dateString": None, "onNull": "was null"}}, {}),  # -> onNull
    # tz designators compute — result normalised to its UTC instant (naive) both
    # sides. Z (UTC), + offset (wall - offset), - offset (wall + offset).
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00Z"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00+05:00"}}, {}),  # -> 05:30Z
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00-08:00"}}, {}),  # -> 18:30Z
    ({"$dateFromString": {"dateString": "2024-01-15T00:30:00+05:00"}}, {}),  # crosses to prev day
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00.123456"}}, {}),  # frac -> defer
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00.5Z"}}, {}),  # frac+Z -> defer
    ({"$dateFromString": {"dateString": "2024-13-01"}}, {}),  # bad month -> defer
    (
        {"$dateFromString": {"dateString": "15/01/2024", "format": "%d/%m/%Y"}},
        {},
    ),  # format -> defer
    ({"$dateFromString": {"dateString": "2024-01-15", "timezone": "America/New_York"}}, {}),  # tz
    # Fixed-offset timezone: a naive string is interpreted in that zone
    # (utc = wall - offset); UTC/GMT aliases and ±HHMM / ±HH:MM forms compute.
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00", "timezone": "+05:00"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15", "timezone": "-08:00"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00", "timezone": "+0530"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00", "timezone": "UTC"}}, {}),
    # A string that already carries an offset ignores the timezone field.
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:00Z", "timezone": "+05:00"}}, {}),
    # $dateFromString `format` (strptime) — numeric-directive subset, built from
    # CPython _strptime's exact per-directive regexes so field matching agrees.
    ({"$dateFromString": {"dateString": "15/01/2024", "format": "%d/%m/%Y"}}, {}),
    ({"$dateFromString": {"dateString": "2024-01-15T10:30:45", "format": "%Y-%m-%dT%H:%M:%S"}}, {}),
    ({"$dateFromString": {"dateString": "20240115", "format": "%Y%m%d"}}, {}),  # adjacent
    ({"$dateFromString": {"dateString": "2024-1-5", "format": "%Y-%m-%d"}}, {}),  # single-digit
    ({"$dateFromString": {"dateString": "68-06-15", "format": "%y-%m-%d"}}, {}),  # 2000s pivot
    ({"$dateFromString": {"dateString": "69-06-15", "format": "%y-%m-%d"}}, {}),  # 1900s pivot
    ({"$dateFromString": {"dateString": "2024-100", "format": "%Y-%j"}}, {}),  # day-of-year
    ({"$dateFromString": {"dateString": "100", "format": "%j"}}, {}),  # default year 1900
    ({"$dateFromString": {"dateString": "date: 2024-01-15", "format": "date: %Y-%m-%d"}}, {}),
    (
        {
            "$dateFromString": {
                "dateString": "2024-01-15",
                "format": "%Y-%m-%d",
                "timezone": "+05:00",
            }
        },
        {},
    ),
    # defers: bad field / leap second / unsupported directive / literal mismatch.
    ({"$dateFromString": {"dateString": "2023-02-29", "format": "%Y-%m-%d"}}, {}),  # -> defer
    (
        {"$dateFromString": {"dateString": "10:30:60", "format": "%H:%M:%S"}},
        {},
    ),  # leap sec -> defer
    ({"$dateFromString": {"dateString": "2024-01-15", "format": "%Y-%m-%d%z"}}, {}),  # %z -> defer
    (
        {"$dateFromString": {"dateString": "2024/01/15", "format": "%Y-%m-%d"}},
        {},
    ),  # mismatch -> defer
    # $dateToString — default format + unambiguous directives. `_DT` is a modern
    # date; a separate date carries non-zero milliseconds for %L.
    ({"$dateToString": {"date": "$d"}}, {"d": _DT}),  # default %Y-%m-%dT%H:%M:%S.%LZ
    ({"$dateToString": {"date": "$d", "format": "%Y-%m-%d"}}, {"d": _DT}),
    ({"$dateToString": {"date": "$d", "format": "%H:%M:%S"}}, {"d": _DT}),
    ({"$dateToString": {"date": "$d", "format": "doy=%j dow=%w iso=%u"}}, {"d": _DT}),
    ({"$dateToString": {"date": "$d", "format": "literal %% pct"}}, {"d": _DT}),
    ({"$dateToString": {"date": "$m"}}, {"m": _mkdate(1_749_000_000_234)}),  # %L = 234
    ({"$dateToString": {"date": 5}}, {}),  # non-datetime -> null
    ({"$dateToString": {"date": "$x"}}, {}),  # missing -> null
    # Named IANA timezone: Rust resolves the DST-correct offset at the instant via
    # chrono-tz (instant->wall-clock is unambiguous, so it matches Python zoneinfo).
    # Curated to post-2007 dates in decade-stable major zones to stay clear of tzdb
    # release skew between chrono-tz's bundled db and CI's Python tzdata.
    ({"$dateToString": {"date": "$d", "timezone": "America/New_York"}}, {"d": _DT}),  # summer EDT
    (
        {
            "$dateToString": {
                "date": "$d",
                "format": "%Y-%m-%d %H:%M",
                "timezone": "America/New_York",
            }
        },
        {"d": datetime.datetime(2023, 1, 15, 16, 30, tzinfo=datetime.timezone.utc)},  # winter EST
    ),
    (
        {"$dateToString": {"date": "$d", "format": "%H:%M", "timezone": "Europe/Dublin"}},
        {"d": datetime.datetime(2023, 7, 15, 16, 30, tzinfo=datetime.timezone.utc)},  # IST +01:00
    ),
    (
        {"$dateToString": {"date": "$d", "format": "%Y-%m-%d %H:%M", "timezone": "Asia/Tokyo"}},
        {"d": _DT},  # JST +09:00 (no DST)
    ),
    ({"$dateToString": {"date": "$d", "timezone": "Not/AZone"}}, {"d": _DT}),  # unknown -> defer
    # Fixed-offset timezone shifts the wall clock before rendering.
    ({"$dateToString": {"date": "$d", "timezone": "+05:30"}}, {"d": _DT}),
    (
        {"$dateToString": {"date": "$d", "timezone": "-0800", "format": "%Y-%m-%d %H:%M"}},
        {"d": _DT},
    ),
    ({"$dateToString": {"date": "$d", "timezone": "UTC"}}, {"d": _DT}),
    ({"$dateToString": {"date": "$d", "format": "%z"}}, {"d": _DT}),  # unknown directive -> defer
    # $range.
    ({"$range": [0, 5]}, {}),
    ({"$range": [0, 10, 2]}, {}),
    ({"$range": [5, 0, -1]}, {}),
    ({"$range": [0, 0]}, {}),
    ({"$range": [0, 3, 0]}, {}),  # step 0 -> defer (Python raises)
    # $strLenBytes.
    ({"$strLenBytes": "héllo"}, {}),  # é is 2 UTF-8 bytes -> 6
    ({"$strLenBytes": "abc"}, {}),
    # $arrayToObject.
    ({"$arrayToObject": [{"k": "a", "v": 1}, {"k": "b", "v": 2}]}, {}),
    ({"$arrayToObject": [["x", 1], ["y", 2]]}, {}),
    ({"$arrayToObject": "$pairs"}, {"pairs": [{"k": "n", "v": 9}]}),
    ({"$arrayToObject": [{"k": 1, "v": 2}]}, {}),  # non-string key -> defer
    # String index / substr / trim.
    ({"$indexOfCP": ["abcabc", "bc"]}, {}),
    ({"$indexOfCP": ["abcabc", "bc", 2]}, {}),
    ({"$indexOfCP": ["abc", "x"]}, {}),
    ({"$indexOfCP": ["héllo", "llo"]}, {}),  # codepoint index 2
    ({"$indexOfCP": ["$s", "x"]}, {}),  # missing -> null
    ({"$indexOfBytes": ["héllo", "llo"]}, {}),  # byte index 3 (é is 2 bytes)
    ({"$indexOfBytes": ["abcabc", "c", 3, 6]}, {}),
    ({"$indexOfBytes": ["abcabc", "b", 2.0]}, {}),  # whole-double start now computes
    ({"$indexOfCP": ["abcabc", "b", 2.0]}, {}),
    # Invalid start/end: Rust defers (None); Python raises 40096 / 40097.
    ({"$indexOfBytes": ["abcabc", "b", 2.5]}, {}),  # fractional -> 40096
    ({"$indexOfBytes": ["abcabc", "b", True]}, {}),  # bool -> 40096
    ({"$indexOfBytes": ["abcabc", "b", -1]}, {}),  # negative -> 40097
    ({"$indexOfCP": ["abcabc", "b", "x"]}, {}),  # non-numeric -> 40096
    ({"$indexOfBytes": ["abcabc", "b", 0, -1]}, {}),  # negative end -> 40097
    ({"$substrBytes": ["hello", 1, 3]}, {}),
    ({"$substrBytes": ["héllo", 0, 1]}, {}),  # "h"
    ({"$substrBytes": ["héllo", 1, 1]}, {}),  # splits é -> invalid utf8 -> defer
    ({"$trim": {"input": "  xx  ", "chars": " "}}, {}),
    ({"$trim": {"input": "xxhixx", "chars": "x"}}, {}),
    ({"$ltrim": {"input": "xxhi", "chars": "x"}}, {}),
    ({"$rtrim": {"input": "hixx", "chars": "x"}}, {}),
    ({"$trim": {"input": "$s"}}, {"s": "  hi  "}),  # default whitespace -> defer
    ({"$trim": {"input": "$s", "chars": " "}}, {"s": None}),  # null input -> null
    ({"$trim": {"input": "--x--", "chars": None}}, {}),  # null chars -> null (both)
    ({"$trim": {"input": "x", "chars": 5}}, {}),  # non-string chars -> defer (50700)
    ({"$ltrim": {"input": "x", "chars": True}}, {}),  # bool chars -> defer
    ({"$rtrim": {"input": 5, "chars": "x"}}, {}),  # non-string input -> defer (50699)
    # $getField / $setField / $zip.
    ({"$getField": "x"}, {"x": 5}),
    ({"$getField": "missing"}, {}),  # absent -> MISSING marker -> Rust defers
    ({"$getField": {"field": "k", "input": "$o"}}, {"o": {"j": 2}}),  # absent field -> defer
    ({"$getField": {"field": "k", "input": "$o"}}, {"o": {"k": None}}),  # present null -> null
    ({"$getField": {"field": "k", "input": "$o"}}, {}),  # input path missing -> defer
    ({"$getField": {"field": "a.b", "input": "$o"}}, {"o": {"a.b": 9, "c": 1}}),
    ({"$getField": {"field": "$fname", "input": "$o"}}, {"fname": "k", "o": {"k": 7}}),
    ({"$getField": {"field": "k", "input": "$o"}}, {"o": None}),  # null input -> null
    ({"$setField": {"field": "y", "input": "$o", "value": 10}}, {"o": {"x": 1}}),
    ({"$setField": {"field": "x", "input": "$o", "value": 9}}, {"o": {"x": 1}}),  # replace
    ({"$setField": {"field": "f", "input": "$o", "value": 1}}, {"o": None}),  # null -> null
    ({"$setField": {"field": "f", "input": "$o", "value": "$$REMOVE"}}, {"o": {"f": 1}}),  # defer
    ({"$zip": {"inputs": [[1, 2], [3, 4]]}}, {}),
    ({"$zip": {"inputs": [[1, 2, 3], [4, 5]]}}, {}),  # min length
    ({"$zip": {"inputs": "$xs"}}, {"xs": [["a", "b"], ["c", "d"]]}),
    ({"$zip": {"inputs": [[1, 2, 3], [4]], "useLongestLength": True}}, {}),
    ({"$zip": {"inputs": [[1, 2, 3], [4]], "useLongestLength": True, "defaults": [0, 0]}}, {}),
    ({"$zip": {"inputs": "$x"}}, {}),  # missing -> null inputs -> null
    # New date-component extractors ($dayOfYear/$week/$isoWeek/$isoDayOfWeek/
    # $isoWeekYear/$millisecond) — bare-date and {date, timezone} forms.
    ({"$dayOfYear": "$d"}, {"d": _DT}),
    ({"$week": "$d"}, {"d": _DT}),
    ({"$isoWeek": "$d"}, {"d": _DT}),
    ({"$isoDayOfWeek": "$d"}, {"d": _DT}),
    ({"$isoWeekYear": "$d"}, {"d": _DT}),
    ({"$millisecond": "$d"}, {"d": _DT}),
    ({"$millisecond": "$d"}, {"d": _mkdate(_DT.timestamp() * 1000 + 123)}),
    # Year-boundary cases (Jan 1 Thursday -> US week 0; Jan 1 next year Friday ->
    # ISO week 53 of the prior ISO year).
    ({"$week": "$d"}, {"d": datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc)}),
    ({"$isoWeek": "$d"}, {"d": datetime.datetime(2027, 1, 1, tzinfo=datetime.timezone.utc)}),
    ({"$isoWeekYear": "$d"}, {"d": datetime.datetime(2027, 1, 1, tzinfo=datetime.timezone.utc)}),
    ({"$dayOfYear": "$d"}, {"d": datetime.datetime(2024, 12, 31, tzinfo=datetime.timezone.utc)}),
    # {date, timezone} object form — fixed offset and named IANA zone crossing a
    # day boundary. (Named-zone cases compute on the Rust side via chrono-tz.)
    ({"$dayOfYear": {"date": "$d", "timezone": "-05:00"}}, {"d": _DT}),
    ({"$isoDayOfWeek": {"date": "$d", "timezone": "+05:30"}}, {"d": _DT}),
    ({"$isoWeek": {"date": "$d", "timezone": "America/New_York"}}, {"d": _DT}),
    # null / non-date operands -> null (both engines).
    ({"$isoWeek": "$x"}, {}),
    ({"$millisecond": "$d"}, {"d": None}),
    # $dateToParts iso8601 both modes + timezone.
    ({"$dateToParts": {"date": "$d", "iso8601": True}}, {"d": _DT}),
    ({"$dateToParts": {"date": "$d", "iso8601": False}}, {"d": _DT}),
    (
        {"$dateToParts": {"date": "$d", "iso8601": True}},
        {"d": datetime.datetime(2027, 1, 1, 2, 0, tzinfo=datetime.timezone.utc)},
    ),
    ({"$dateToParts": {"date": "$d", "iso8601": True, "timezone": "-05:00"}}, {"d": _DT}),
    ({"$dateToParts": {"date": "$d", "iso8601": True, "timezone": "America/New_York"}}, {"d": _DT}),
    # Date arithmetic.
    ({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 5}}, {"d": _DT}),
    ({"$dateAdd": {"startDate": "$d", "unit": "month", "amount": 1}}, {"d": _DT}),
    ({"$dateAdd": {"startDate": "$d", "unit": "year", "amount": 1}}, {"d": _DT}),
    # Jan 31 + 1 month clamps to Feb 28
    (
        {"$dateAdd": {"startDate": "$d", "unit": "month", "amount": 1}},
        {"d": datetime.datetime(2026, 1, 31, tzinfo=datetime.timezone.utc)},
    ),
    ({"$dateAdd": {"startDate": "$x", "unit": "day", "amount": 5}}, {}),  # null -> null
    ({"$dateSubtract": {"startDate": "$d", "unit": "hour", "amount": 3}}, {"d": _DT}),
    ({"$dateSubtract": {"startDate": "$d", "unit": "month", "amount": 2}}, {"d": _DT}),
    (
        {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "day"}},
        {"a": _DT, "b": _mkdate(_DT.timestamp() * 1000 + 3 * 86_400_000)},
    ),
    (
        {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "year"}},
        {"a": datetime.datetime(2020, 6, 5, tzinfo=datetime.timezone.utc), "b": _DT},
    ),
    (
        {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "month"}},
        {"a": datetime.datetime(2026, 1, 15, tzinfo=datetime.timezone.utc), "b": _DT},
    ),
    ({"$dateTrunc": {"date": "$d", "unit": "hour"}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "month"}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "week"}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "year", "binSize": 5}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "minute", "binSize": 15}}, {"d": _DT}),
    # Whole-double amount / binSize now computes on both engines.
    ({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 5.0}}, {"d": _DT}),
    ({"$dateSubtract": {"startDate": "$d", "unit": "hour", "amount": 3.0}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "year", "binSize": 2.0}}, {"d": _DT}),
    # Invalid amount / binSize: Rust defers (None); Python raises 5166405 / 5439017.
    ({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 2.5}}, {"d": _DT}),
    ({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": True}}, {"d": _DT}),
    ({"$dateSubtract": {"startDate": "$d", "unit": "day", "amount": True}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "day", "binSize": True}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "day", "binSize": 2.5}}, {"d": _DT}),
    ({"$dateTrunc": {"date": "$d", "unit": "day", "binSize": -1}}, {"d": _DT}),
    # Cases that should defer (rust None -> skipped):
    ({"$toUpper": "café"}, {}),  # non-ASCII
    ({"$dateToString": {"date": "$d", "format": "%Y"}}, {"d": "x"}),
    # Arithmetic type errors (mongod raises; Python raises, Rust must
    # defer — a Rust VALUE here is a divergence the loop fails on).
    ({"$multiply": [2, "$s"]}, {"s": "nope"}),  # string -> Python raises
    ({"$multiply": [2, True]}, {}),  # bool is not numeric -> Python raises
    ({"$multiply": ["$s"]}, {"s": "x"}),  # single non-numeric -> raises
    ({"$add": [2, "$s"]}, {"s": "nope"}),
    ({"$add": [2, True]}, {}),
    ({"$add": ["$s"]}, {"s": "x"}),  # single-arg $add type-checks too
    ({"$subtract": [2, "$s"]}, {"s": "nope"}),
    ({"$subtract": [True, 1]}, {}),
    ({"$divide": [2, "$s"]}, {"s": "nope"}),
    ({"$divide": [2, True]}, {}),
    ({"$divide": [2, 0]}, {}),  # mongod: can't $divide by zero -> raises
    ({"$mod": [2, "$s"]}, {"s": "nope"}),
    ({"$mod": [2, 0]}, {}),  # mongod: can't $mod by zero -> raises
    # Null still propagates BEFORE type checks (both engines return null).
    ({"$multiply": [None, "$s"]}, {"s": "nope"}),
    ({"$add": [None, True]}, {}),
    # bool where an int is expected: mongod rejects (bool is not a number),
    # Python raises the exact code, Rust must defer (a Rust VALUE = divergence).
    ({"$round": [1.5, True]}, {}),
    ({"$trunc": [1.5, True]}, {}),
    ({"$arrayElemAt": [[10, 20, 30], True]}, {}),
    ({"$slice": [[1, 2, 3, 4], True]}, {}),
    ({"$slice": [[1, 2, 3, 4], True, 2]}, {}),
    ({"$slice": [[1, 2, 3, 4], 1, True]}, {}),
    ({"$sortArray": {"input": [3, 1, 2], "sortBy": True}}, {}),
    ({"$substrCP": ["hello", True, 2]}, {}),
    ({"$substrCP": ["hello", 1, True]}, {}),
    ({"$substrBytes": ["hello", True, 2]}, {}),
    ({"$substrBytes": ["hello", 1, True]}, {}),
    ({"$substr": ["hello", True, 2]}, {}),
    ({"$substr": ["hello", 1, True]}, {}),
    # $substr aliases $substrBytes (byte-based) on both engines — ASCII computes.
    ({"$substr": ["hello", 1, 3]}, {}),
    ({"$range": [0, True]}, {}),
    ({"$range": [True, 5]}, {}),
    ({"$range": [0, 5, True]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 2, True]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 2, 0, True]}, {}),
    # whole-number double index: mongod (and now both engines) accept it and
    # compute; a fractional double is rejected (Python raises, Rust defers).
    ({"$arrayElemAt": [[10, 20, 30], 2.0]}, {}),
    ({"$arrayElemAt": [[10, 20, 30], -1.0]}, {}),
    ({"$arrayElemAt": [[10, 20, 30], 2.7]}, {}),
    ({"$slice": [[1, 2, 3, 4], 2.0]}, {}),
    ({"$slice": [[1, 2, 3, 4], 2.7]}, {}),
    ({"$slice": [[1, 2, 3, 4], 1.0, 2.0]}, {}),
    ({"$slice": [[1, 2, 3, 4], 1.7, 2]}, {}),
    ({"$slice": [[1, 2, 3, 4], 1, 1.7]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 2, 0.0]}, {}),
    ({"$indexOfArray": [[1, 2, 3], 2, 0.7]}, {}),
    # whole-double acceptance extends to substrCP / range / round / trunc.
    ({"$substrCP": ["hello", 1.0, 2]}, {}),
    ({"$substrCP": ["hello", 1.7, 2]}, {}),
    ({"$substrCP": ["hello", 1, 1.7]}, {}),
    ({"$range": [0.0, 5.0, 1.0]}, {}),
    ({"$range": [0.7, 5]}, {}),
    ({"$range": [0, 5.7]}, {}),
    ({"$range": [0, 5, 1.7]}, {}),
    ({"$round": [3.14159, 2.0]}, {}),
    ({"$round": [3.14159, 2.7]}, {}),
    ({"$trunc": [3.14159, 2.0]}, {}),
    ({"$trunc": [3.14159, 2.7]}, {}),
    # $substrBytes splitting a UTF-8 char: Python raises 28656/28657, the Rust
    # core defers (its slice isn't valid UTF-8). Clean boundaries compute equally.
    ({"$substrBytes": ["héllo", 0, 2]}, {}),
    ({"$substrBytes": ["héllo", 2, 3]}, {}),
    ({"$substr": ["héllo", 0, 2]}, {}),
    ({"$substrBytes": ["héllo", 0, 3]}, {}),
    ({"$substrBytes": ["héllo", 3, 2]}, {}),
    # continuation-byte start / end-split rejected even for an empty range: the
    # core must defer (not return "") — the case a fuzz run surfaced.
    ({"$substrBytes": ["héllo", 2, 0]}, {}),
    ({"$substrBytes": ["héllo", 1, 1]}, {}),
    ({"$substrBytes": ["éa😀ézé", 1, 0]}, {}),
    ({"$substrBytes": ["héllo", 1, 0]}, {}),
    ({"$substrBytes": ["héllo", 99, 0]}, {}),
    # negative start rejected on both ops (50752 / 34455); negative length
    # rejected on $substrCP (34454) but fine on $substrBytes (to end).
    ({"$substrBytes": ["abcde", -1, 2]}, {}),
    ({"$substrBytes": ["abcde", 1, -1]}, {}),
    ({"$substrCP": ["abcde", -1, 2]}, {}),
    ({"$substrCP": ["abcde", 1, -1]}, {}),
    # $substrBytes truncates a double toward zero -- both engines compute equally.
    ({"$substrBytes": ["abcde", 1.7, 2]}, {}),
    ({"$substrBytes": ["abcde", 0.9, 3]}, {}),
    ({"$substrBytes": ["abcde", 1, 2.9]}, {}),
    ({"$substrBytes": ["abcde", 1, -1.7]}, {}),
    ({"$substrBytes": ["abcde", -1.7, 2]}, {}),
    # $pow: integer/whole-double compute on both engines; non-numeric / bool /
    # zero-base-negative-exponent raise on Python and defer on Rust. (The
    # negative-base-fractional NaN case is excluded — NaN != NaN breaks the
    # equality assert; it's covered in the unit/integration tests via isnan.)
    ({"$pow": [-2, 3]}, {}),
    ({"$pow": [2.0, 3]}, {}),
    ({"$pow": [2, 10]}, {}),
    ({"$pow": ["x", 2]}, {}),
    ({"$pow": [2, True]}, {}),
    ({"$pow": [0, -1]}, {}),
]


@pytest.mark.parametrize("expr,doc", CURATED)
def test_curated_parity(expr, doc):
    doc = bson.decode(bson.encode(doc))
    rust = _rust_eval(expr, doc)
    if rust is None:
        return
    py = _bson_norm(_pure.evaluate(expr, doc))
    assert rust == py, f"rust={rust!r} pure={py!r} expr={expr}"


def test_rand_shape_parity():
    # $rand is non-deterministic — the engines can't agree bit-for-bit, only on
    # the shape: a float in [0, 1). Verify both produce that (and that the Rust
    # engine evaluates it, not defers), and that a malformed arg defers/raises.
    for _ in range(64):
        rust = _rust_eval({"$rand": {}}, {})
        assert isinstance(rust, float) and 0.0 <= rust < 1.0, f"rust $rand={rust!r}"
        py = _pure.evaluate({"$rand": {}}, {})
        assert isinstance(py, float) and 0.0 <= py < 1.0, f"pure $rand={py!r}"
    # Non-empty argument: Rust defers (None), Python raises a parse error.
    assert _rust_eval({"$rand": {"x": 1}}, {}) is None
    with pytest.raises(_pure.ExpressionError):
        _pure.evaluate({"$rand": {"x": 1}}, {})


def _rand_scalar(rng):
    return rng.choice(
        [rng.randint(-20, 20), round(rng.uniform(-9, 9), 2), "a", "bb", "z", True, False, None]
    )


def _rand_doc(rng):
    d = {}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.4:
            d[f] = [rng.randint(0, 9) for _ in range(rng.randint(0, 3))]
        else:
            d[f] = _rand_scalar(rng)
    return d


def _rand_operand(rng, depth):
    r = rng.random()
    if depth <= 0 or r < 0.5:
        return rng.choice(
            [rng.randint(-9, 9), round(rng.uniform(-5, 5), 1), "$a", "$b", "$c", "lit", True, False]
        )
    return _rand_expr(rng, depth - 1)


def _rand_expr(rng, depth):
    op = rng.choice(
        [
            "$eq",
            "$ne",
            "$gt",
            "$gte",
            "$lt",
            "$lte",
            "$add",
            "$subtract",
            "$multiply",
            "$divide",
            "$mod",
            "$and",
            "$or",
            "$not",
            "$cond",
            "$ifNull",
        ]
    )
    if op in ("$and", "$or"):
        return {op: [_rand_operand(rng, depth) for _ in range(rng.randint(1, 3))]}
    if op == "$not":
        return {op: [_rand_operand(rng, depth)]}
    if op == "$cond":
        return {op: [_rand_operand(rng, depth) for _ in range(3)]}
    if op in ("$add", "$multiply"):
        return {op: [_rand_operand(rng, depth) for _ in range(rng.randint(1, 3))]}
    # binary ops
    return {op: [_rand_operand(rng, depth), _rand_operand(rng, depth)]}


def test_index_math_fuzz():
    """Stress $slice / $substrCP / $indexOfArray index arithmetic (the riskiest
    part — negative indices, out-of-range, clamping) against pure Python."""
    rng = random.Random(0x51CE)
    handled = 0
    for _ in range(6000):
        arr = [rng.randint(0, 4) for _ in range(rng.randint(0, 6))]
        s = "".join(rng.choice("abcde") for _ in range(rng.randint(0, 6)))
        lo, hi = rng.randint(-8, 8), rng.randint(-8, 8)
        expr = rng.choice(
            [
                {"$slice": [arr, lo]},
                {"$slice": [arr, lo, hi]},
                {"$substrCP": [s, lo, hi]},
                {"$indexOfArray": [arr, rng.randint(0, 4)]},
                {"$indexOfArray": [arr, rng.randint(0, 4), lo]},
                {"$indexOfArray": [arr, rng.randint(0, 4), lo, hi]},
            ]
        )
        expr = bson.decode(bson.encode({"e": expr}))["e"]
        rust = _rust_eval(expr, {})
        if rust is None:
            continue
        handled += 1
        py = _pure.evaluate(expr, {})
        assert rust == py, f"divergence: rust={rust!r} pure={py!r} expr={expr}"
    assert handled > 2000, f"expected many handled cases, only {handled}"


def test_date_extractor_fuzz():
    """Validate civil-date arithmetic ($year/$month/$dayOfMonth/$hour/$minute/
    $second/$dayOfWeek) across a wide instant range — including pre-epoch
    (negative millis) — against Python's datetime."""
    rng = random.Random(0xDA7E)
    epoch = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
    ops = [
        "$year",
        "$month",
        "$dayOfMonth",
        "$hour",
        "$minute",
        "$second",
        "$millisecond",
        "$dayOfWeek",
        "$dayOfYear",
        "$week",
        "$isoWeek",
        "$isoDayOfWeek",
        "$isoWeekYear",
    ]
    for _ in range(4000):
        # ~ year 1900 .. 2400 (spans negative/positive millis)
        ms = rng.randint(-2_200_000_000_000, 13_600_000_000_000)
        dt = epoch + datetime.timedelta(milliseconds=ms)
        doc = bson.decode(bson.encode({"d": dt}))
        for op in ops:
            expr = {op: "$d"}
            rust = _rust_eval(expr, doc)
            if rust is None:
                continue
            py = _pure.evaluate(expr, doc)
            assert rust == py, f"{op}: rust={rust} pure={py} ms={ms} dt={doc['d']}"


def test_date_from_string_strptime_fuzz():
    """$dateFromString `format` (strptime): the Rust regex-built parser must match
    Python's datetime.strptime exactly wherever Rust computes (else it defers).
    Mixes valid strftime-rendered inputs with random junk so both the compute and
    the defer/raise paths are exercised."""
    rng = random.Random(0x57717D)
    fmts = [
        "%Y-%m-%d",
        "%d/%m/%Y",
        "%Y-%m-%dT%H:%M:%S",
        "%Y%m%d",
        "%m-%d-%Y",
        "%Y-%j",
        "%y-%m-%d",
        "at %Y-%m-%d %H:%M",
        "%H:%M:%S",
    ]
    for _ in range(6000):
        fmt = rng.choice(fmts)
        if rng.random() < 0.8:
            try:
                dt = datetime.datetime(
                    rng.randint(1, 9999),
                    rng.randint(1, 12),
                    rng.randint(1, 28),
                    rng.randint(0, 23),
                    rng.randint(0, 59),
                    rng.randint(0, 59),
                )
                inp = dt.strftime(fmt)
            except ValueError:
                continue
        else:
            inp = "".join(rng.choice("0123456789-/T: ") for _ in range(rng.randint(3, 12)))
        expr = {"$dateFromString": {"dateString": inp, "format": fmt}}
        rust = _rust_eval(expr, {})
        if rust is None:
            continue  # Rust deferred -> Python (compute or raise) handles it
        py = _bson_norm(_pure.evaluate(expr, {}))
        assert rust == py, f"rust={rust!r} pure={py!r} inp={inp!r} fmt={fmt!r}"


def test_date_arithmetic_fuzz():
    """$dateAdd/$dateSubtract/$dateDiff/$dateTrunc over random instants, units,
    amounts and binSizes, against Python (calendar + delta arithmetic)."""
    rng = random.Random(0xDA7EA)
    units = ["year", "quarter", "month", "week", "day", "hour", "minute", "second", "millisecond"]
    # ~ year 1950..2200, so add/subtract stays well within datetime range.
    lo, hi = -631_152_000_000, 7_258_118_400_000
    for _ in range(8000):
        d1 = _mkdate(rng.randint(lo, hi))
        d2 = _mkdate(rng.randint(lo, hi))
        unit = rng.choice(units)
        kind = rng.choice(["add", "sub", "diff", "trunc"])
        amt = rng.randint(-500, 500)
        if kind == "add":
            expr = {"$dateAdd": {"startDate": "$a", "unit": unit, "amount": amt}}
        elif kind == "sub":
            expr = {"$dateSubtract": {"startDate": "$a", "unit": unit, "amount": amt}}
        elif kind == "diff":
            expr = {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": unit}}
        else:
            expr = {"$dateTrunc": {"date": "$a", "unit": unit, "binSize": rng.randint(1, 7)}}
        doc = bson.decode(bson.encode({"a": d1, "b": d2}))
        expr = bson.decode(bson.encode({"e": expr}))["e"]
        rust = _rust_eval(expr, doc)
        if rust is None:
            continue
        try:
            py = _pure.evaluate(expr, doc)
        except Exception:
            pytest.fail(f"rust={rust!r} but pure raised; expr={expr} a={d1} b={d2}")
        assert rust == py, f"rust={rust!r} pure={py!r} expr={expr} a={d1} b={d2}"


def test_zip_fuzz():
    """$zip over random ragged arrays, with useLongestLength on/off and random
    defaults, against Python."""
    rng = random.Random(0x21B)
    for _ in range(3000):
        inputs = [
            [rng.randint(0, 9) for _ in range(rng.randint(0, 4))] for _ in range(rng.randint(0, 3))
        ]
        spec = {"inputs": inputs}
        if rng.random() < 0.5:
            spec["useLongestLength"] = rng.choice([True, False])
        if rng.random() < 0.4:
            spec["defaults"] = [rng.randint(-1, -1) for _ in inputs]
        expr = bson.decode(bson.encode({"e": {"$zip": spec}}))["e"]
        rust = _rust_eval(expr, {})
        if rust is None:
            continue
        try:
            py = _pure.evaluate(expr, {})
        except Exception:
            pytest.fail(f"rust={rust!r} but pure raised; expr={expr}")
        assert rust == py, f"rust={rust!r} pure={py!r} expr={expr}"


def test_string_index_fuzz():
    """$indexOfCP / $indexOfBytes / $substrBytes / $substrCP over random strings
    (incl. multibyte) and random start/end/length, against Python where the Rust
    path doesn't defer (broken-UTF-8 substrBytes boundaries defer)."""
    rng = random.Random(0x57B1)
    alphabet = "abcé😀z "
    for _ in range(6000):
        s = "".join(rng.choice(alphabet) for _ in range(rng.randint(0, 8)))
        needle = "".join(rng.choice(alphabet) for _ in range(rng.randint(0, 2)))
        lo, hi = rng.randint(-6, 10), rng.randint(-6, 10)
        expr = rng.choice(
            [
                {"$indexOfCP": [s, needle]},
                {"$indexOfCP": [s, needle, lo]},
                {"$indexOfCP": [s, needle, lo, hi]},
                {"$indexOfBytes": [s, needle]},
                {"$indexOfBytes": [s, needle, lo, hi]},
                {"$substrBytes": [s, lo, hi]},
                {"$substrCP": [s, lo, hi]},
            ]
        )
        expr = bson.decode(bson.encode({"e": expr}))["e"]
        rust = _rust_eval(expr, {})
        if rust is None:
            continue
        try:
            py = _pure.evaluate(expr, {})
        except Exception:
            pytest.fail(f"rust={rust!r} but pure raised; expr={expr}")
        assert rust == py, f"rust={rust!r} pure={py!r} expr={expr}"


def test_math_and_range_fuzz():
    """$abs/$floor/$ceil/$sqrt over random ints & floats, and $range over random
    small bounds, against Python wherever the Rust path doesn't defer."""
    rng = random.Random(0x4A7B)
    for _ in range(5000):
        v = rng.choice(
            [
                rng.randint(-10000, 10000),
                round(rng.uniform(-1000, 1000), 3),
                rng.choice([0, 0.0, -0.0, 2**40, -(2**31)]),
            ]
        )
        for op in ("$abs", "$floor", "$ceil", "$sqrt"):
            expr = {op: v}
            rust = _rust_eval(expr, {})
            if rust is None:
                continue
            try:
                py = _pure.evaluate(expr, {})
            except Exception:
                pytest.fail(f"{op}: rust={rust!r} but pure raised; v={v!r}")
            assert rust == py, f"{op}: rust={rust!r} pure={py!r} v={v!r}"
        # $range
        lo, hi = rng.randint(-20, 20), rng.randint(-20, 20)
        step = rng.choice([1, 2, 3, -1, -2])
        expr = {"$range": [lo, hi, step]}
        rust = _rust_eval(expr, {})
        if rust is not None:
            assert rust == _pure.evaluate(expr, {}), f"$range lo={lo} hi={hi} step={step}"


def test_conversion_fuzz():
    """$toInt / $toDouble / $toBool / $toString over a mix of scalar types,
    checked against Python wherever the Rust path doesn't defer."""
    values = [
        0,
        1,
        -7,
        2**40,
        Int64(5),
        Int64(-3),
        0.0,
        3.9,
        -3.9,
        1e10,
        2.5,
        True,
        False,
        None,
        "",
        "abc",
        "12",
        ObjectId(),
    ]
    for v in values:
        doc = bson.decode(bson.encode({"v": v}))
        for op in ("$toInt", "$toDouble", "$toBool", "$toString"):
            expr = {op: "$v"}
            rust = _rust_eval(expr, doc)
            if rust is None:
                continue
            try:
                py = _pure.evaluate(expr, doc)
            except Exception:
                pytest.fail(f"{op}: rust={rust!r} but pure raised; v={v!r}")
            assert rust == py, f"{op}: rust={rust!r} pure={py!r} v={v!r}"


def test_randomised_fuzz_parity():
    rng = random.Random(0xE5DA)
    handled = 0
    for _ in range(8000):
        doc = bson.decode(bson.encode(_rand_doc(rng)))
        expr = bson.decode(bson.encode({"e": _rand_expr(rng, 3)}))["e"]
        rust = _rust_eval(expr, doc)
        if rust is None:
            continue
        try:
            py = _pure.evaluate(expr, doc)
        except Exception:
            # Rust produced a value where Python raises -> a real divergence.
            pytest.fail(f"rust={rust!r} but pure raised; expr={expr} doc={doc}")
        handled += 1
        assert rust == py, f"divergence: rust={rust!r} pure={py!r} expr={expr} doc={doc}"
    assert handled > 1000, f"expected many handled cases, only {handled}"


@pytest.mark.parametrize(
    "expr,code",
    [
        ({"$size": 5}, 17124),
        ({"$arrayElemAt": [5, 0]}, 28689),
        ({"$in": [1, 5]}, 40081),
        ({"$indexOfArray": [5, 1]}, 40090),
        ({"$setUnion": [5]}, 17043),
        ({"$setIntersection": [5]}, 17047),
        ({"$setDifference": [5, 6]}, 17048),
        ({"$setIsSubset": [5, 6]}, 17046),
        ({"$anyElementTrue": 5}, 17041),
        ({"$allElementsTrue": 5}, 17040),
        ({"$mergeObjects": [5]}, 40400),
        ({"$range": ["a", "b"]}, 34443),
    ],
)
def test_array_set_typeguard_defers_and_raises(expr, code):
    # A non-array/non-object argument to these operators: Rust must *defer* (the
    # raw evaluate returns None) so the pure engine raises mongod's exact
    # Location code — checking the raw result, not `_rust_eval`, because a
    # computed BSON null would also decode to Python None and hide a silent
    # accept (as it did for $arrayElemAt before the Rust fix).
    doc = bson.decode(bson.encode({"_id": 1}))
    expr = bson.decode(bson.encode({"e": expr}))["e"]
    raw = _rust.evaluate(bson.encode(doc), bson.encode({"e": expr}), bson.encode({}))
    assert raw is None
    with pytest.raises(_pure.ExpressionError) as exc:
        _pure.evaluate(expr, doc)
    assert exc.value.code == code


@pytest.mark.parametrize(
    "expr,code",
    [
        ({"$regexMatch": {"input": 5, "regex": "a"}}, 51104),
        ({"$regexFind": {"input": 5, "regex": "a"}}, 51104),
        ({"$regexFindAll": {"input": 5, "regex": "a"}}, 51104),
        ({"$indexOfBytes": [5, "a"]}, 40091),
        ({"$binarySize": 5}, 51276),
        ({"$bsonSize": 5}, 31393),
    ],
)
def test_string_typeguard_defers_and_raises(expr, code):
    # Non-string argument to these operators: Rust must defer (raw evaluate None)
    # so the pure engine raises mongod's exact code. The regex ops previously
    # silently returned false/null/[] on both engines.
    doc = bson.decode(bson.encode({"_id": 1}))
    expr = bson.decode(bson.encode({"e": expr}))["e"]
    raw = _rust.evaluate(bson.encode(doc), bson.encode({"e": expr}), bson.encode({}))
    assert raw is None
    with pytest.raises(_pure.ExpressionError) as exc:
        _pure.evaluate(expr, doc)
    assert exc.value.code == code


@pytest.mark.parametrize(
    "expr,code",
    [
        ({"$dateToString": {"date": "x"}}, 16006),
        ({"$dateToParts": {"date": "x"}}, 16006),
        ({"$dateFromString": {"dateString": 5}}, 241),
        ({"$let": {"vars": {}, "in": "$$x"}}, 17276),
        ({"$switch": {"branches": []}}, 40068),
        ({"$ifNull": [1]}, 1257300),
        ({"$getField": {"field": 5, "input": {}}}, 5654602),
        ({"$setField": {"field": 5, "input": {}, "value": 1}}, 4161107),
        ({"$sortArray": {"input": [1], "sortBy": "x"}}, 2942507),
        ({"$convert": {"input": 5}}, 9),
        ({"$dateDiff": {"startDate": _DT}}, 5166304),
    ],
)
def test_date_misc_typeguard_defers_and_raises(expr, code):
    # Date/misc operator error cases: Rust must defer (raw evaluate None) so the
    # pure engine raises mongod's exact code. $dateToString and $dateDiff missing
    # endDate were silent accepts.
    doc = bson.decode(bson.encode({"_id": 1}))
    expr = bson.decode(bson.encode({"e": expr}))["e"]
    raw = _rust.evaluate(bson.encode(doc), bson.encode({"e": expr}), bson.encode({}))
    assert raw is None
    with pytest.raises(_pure.ExpressionError) as exc:
        _pure.evaluate(expr, doc)
    assert exc.value.code == code

"""`group_shapes.py` cases: accumulators, group keys, `$bucket`,
`$bucketAuto` and `$sortByCount`."""

from __future__ import annotations

import datetime

from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

NAN = float("nan")
INF = float("inf")
WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5, 678000)

#: The default documents: two groups (`g`), numbers of every type under `n`,
#: and a mix of other values under `v`.
DOCS: list[dict] = [
    {"_id": 1, "g": "a", "n": 1, "v": "x", "o": {"p": 1}, "arr": [1, 2]},
    {"_id": 2, "g": "a", "n": Int64(2), "v": None, "o": {"q": 2}, "arr": [3]},
    {"_id": 3, "g": "a", "n": 2.5, "v": [1, 2], "o": None, "arr": []},
    {"_id": 4, "g": "b", "n": Decimal128("4.25"), "v": {"k": 1}, "o": {"p": 3, "r": 1}},
    {"_id": 5, "g": "b", "n": "7", "v": True, "o": 5},
    {"_id": 6, "g": "b", "v": 3, "arr": [[1], 2]},
    {"_id": 7, "g": "c", "n": None, "v": Int64(3), "o": {"p": None}},
]


def _g(**accs: object) -> list:
    """A `$group` of everything into one bucket, with these accumulators."""
    return [{"$group": {"_id": None, **accs}}]


def _gg(**accs: object) -> list:
    """A `$group` by `g`, sorted by group."""
    return [{"$group": {"_id": "$g", **accs}}, {"$sort": {"_id": 1}}]


def _vals(values: list, field: str = "x") -> list:
    """Documents holding `values` under `field`, in order."""
    return [{"_id": i, field: v} for i, v in enumerate(values)]


NUMS = [1, Int64(2), 3.5, Decimal128("4.5")]
INTS = [1, 2, 3]
LONGS = [Int64(1), Int64(2)]
MIXED = [1, "a", None, True, [1, 2], {"a": 1}, 2.0]
HUGE = [Int64(9223372036854775807), Int64(1)]
NANS = [1, NAN, 2]
INFS = [INF, -INF]
DECS = [Decimal128("0.1"), Decimal128("0.2"), Decimal128("NaN")]

CASES: list[tuple] = [
    # --- $sum / $avg ----------------------------------------------------------------
    ("sum field", _gg(s={"$sum": "$n"})),
    ("avg field", _gg(a={"$avg": "$n"})),
    ("sum ints", _g(s={"$sum": "$x"}), _vals(INTS)),
    ("sum longs", _g(s={"$sum": "$x"}), _vals(LONGS)),
    ("sum overflow to double", _g(s={"$sum": "$x"}), _vals(HUGE)),
    ("sum int overflow to long", _g(s={"$sum": "$x"}), _vals([2147483647, 1])),
    ("sum mixed types", _g(s={"$sum": "$x"}), _vals(MIXED)),
    ("sum decimal", _g(s={"$sum": "$x"}), _vals(DECS)),
    ("sum nan", _g(s={"$sum": "$x"}), _vals(NANS)),
    ("sum infinities", _g(s={"$sum": "$x"}), _vals(INFS)),
    ("sum all numbers", _g(s={"$sum": "$x"}), _vals(NUMS)),
    ("sum none numeric", _g(s={"$sum": "$x"}), _vals(["a", None])),
    ("sum empty", _g(s={"$sum": "$x"}), _vals([])),
    ("sum array value", _g(s={"$sum": "$x"}), _vals([[1, 2], [3]])),
    ("avg ints", _g(a={"$avg": "$x"}), _vals(INTS)),
    ("avg longs", _g(a={"$avg": "$x"}), _vals(HUGE)),
    ("avg decimal", _g(a={"$avg": "$x"}), _vals(DECS[:2])),
    ("avg mixed", _g(a={"$avg": "$x"}), _vals(MIXED)),
    ("avg none numeric", _g(a={"$avg": "$x"}), _vals(["a", None])),
    ("avg nan", _g(a={"$avg": "$x"}), _vals(NANS)),
    ("avg all numbers", _g(a={"$avg": "$x"}), _vals(NUMS)),
    ("sum constant 1", _gg(c={"$sum": 1})),
    ("sum constant long", _gg(c={"$sum": Int64(5)})),
    ("sum constant double", _gg(c={"$sum": 1.5})),
    ("sum constant decimal", _gg(c={"$sum": Decimal128("0.1")})),
    ("sum constant string", _gg(c={"$sum": "x"})),
    ("sum constant null", _gg(c={"$sum": None})),
    ("sum constant literal", _gg(c={"$sum": {"$literal": 2}})),
    ("sum constant big int", _gg(c={"$sum": 2147483647})),
    ("sum constant huge long", _gg(c={"$sum": Int64(4611686018427387904)})),
    ("avg constant", _gg(c={"$avg": 5})),
    ("avg constant long", _gg(c={"$avg": Int64(5)})),
    ("avg constant decimal", _gg(c={"$avg": Decimal128("2.5")})),
    ("sum expression", _gg(c={"$sum": {"$multiply": ["$_id", 2]}})),
    ("sum array of args", _gg(c={"$sum": ["$_id", 1]})),
    # --- $min / $max / $first / $last -------------------------------------------------
    ("min field", _gg(m={"$min": "$v"})),
    ("max field", _gg(m={"$max": "$v"})),
    ("min mixed", _g(m={"$min": "$x"}), _vals(MIXED)),
    ("max mixed", _g(m={"$max": "$x"}), _vals(MIXED)),
    ("min nan", _g(m={"$min": "$x"}), _vals(NANS)),
    ("max nan", _g(m={"$max": "$x"}), _vals(NANS)),
    ("min all null", _g(m={"$min": "$x"}), _vals([None, None])),
    ("max missing", _g(m={"$max": "$nope"})),
    ("min minkey maxkey", _g(m={"$min": "$x"}, M={"$max": "$x"}), _vals([MinKey(), 1, MaxKey()])),
    ("min numbers equal", _g(m={"$min": "$x"}), _vals([1, 1.0, Int64(1)])),
    ("max numbers equal", _g(m={"$max": "$x"}), _vals([Decimal128("1"), 1.0, 1])),
    ("first last", _gg(f={"$first": "$v"}, l={"$last": "$v"})),
    ("first missing", _g(f={"$first": "$nope"})),
    ("last missing", _gg(l={"$last": "$o.q"})),
    ("first expression error", _g(f={"$first": {"$ln": 0}})),
    # --- $push / $addToSet / $mergeObjects ---------------------------------------------
    ("push", _gg(p={"$push": "$v"})),
    ("push missing", _gg(p={"$push": "$o.q"})),
    ("addToSet numbers", _g(s={"$addToSet": "$x"}), _vals([1, 1.0, Int64(1), 2])),
    (
        "addToSet documents order",
        _g(s={"$addToSet": "$x"}),
        _vals([{"a": 1, "b": 2}, {"b": 2, "a": 1}]),
    ),
    ("addToSet nan", _g(s={"$addToSet": "$x"}), _vals([NAN, Decimal128("NaN")])),
    ("addToSet null missing", _g(s={"$addToSet": "$x"}), [{"_id": 1, "x": None}, {"_id": 2}]),
    ("mergeObjects", _gg(m={"$mergeObjects": "$o"})),
    ("mergeObjects null", _g(m={"$mergeObjects": "$x"}), _vals([None, {"a": 1}])),
    ("mergeObjects not object", _g(m={"$mergeObjects": "$x"}), _vals([{"a": 1}, 5])),
    ("mergeObjects missing", _g(m={"$mergeObjects": "$nope"})),
    ("mergeObjects override", _g(m={"$mergeObjects": "$x"}), _vals([{"a": 1, "b": 1}, {"a": 2}])),
    # --- $stdDev* / $count ----------------------------------------------------------
    ("stdDevPop", _gg(s={"$stdDevPop": "$n"})),
    ("stdDevSamp", _gg(s={"$stdDevSamp": "$n"})),
    ("stdDevSamp one", _g(s={"$stdDevSamp": "$x"}), _vals([5])),
    ("stdDevPop decimal", _g(s={"$stdDevPop": "$x"}), _vals(DECS[:2])),
    ("stdDevPop nan", _g(s={"$stdDevPop": "$x"}), _vals(NANS)),
    ("stdDevPop inf", _g(s={"$stdDevPop": "$x"}), _vals([1, INF])),
    ("stdDevPop none numeric", _g(s={"$stdDevPop": "$x"}), _vals(["a"])),
    ("count", _gg(c={"$count": {}})),
    ("count with arg", _gg(c={"$count": {"x": 1}})),
    ("count not object", _gg(c={"$count": 1})),
    # --- $median / $percentile ---------------------------------------------------------
    ("median", _gg(m={"$median": {"input": "$n", "method": "approximate"}})),
    (
        "percentile",
        _gg(p={"$percentile": {"input": "$n", "p": [0.5, 0.9], "method": "approximate"}}),
    ),
    (
        "percentile ints",
        _g(p={"$percentile": {"input": "$x", "p": [0, 0.25, 1], "method": "approximate"}}),
        _vals(INTS),
    ),
    (
        "median decimal",
        _g(m={"$median": {"input": "$x", "method": "approximate"}}),
        _vals(DECS[:2]),
    ),
    ("median nan", _g(m={"$median": {"input": "$x", "method": "approximate"}}), _vals(NANS)),
    ("median none", _g(m={"$median": {"input": "$x", "method": "approximate"}}), _vals(["a"])),
    ("median empty", _g(m={"$median": {"input": "$nope", "method": "approximate"}})),
    ("median not object", _g(m={"$median": "$x"})),
    ("median array", _g(m={"$median": ["$x"]})),
    ("median missing input", _g(m={"$median": {"method": "approximate"}})),
    ("median missing method", _g(m={"$median": {"input": "$x"}})),
    ("median bad method", _g(m={"$median": {"input": "$x", "method": "exact"}})),
    ("median method number", _g(m={"$median": {"input": "$x", "method": 1}})),
    ("median unknown field", _g(m={"$median": {"input": "$x", "method": "approximate", "z": 1}})),
    ("median with p", _g(m={"$median": {"input": "$x", "p": [0.5], "method": "approximate"}})),
    ("percentile missing p", _g(p={"$percentile": {"input": "$x", "method": "approximate"}})),
    (
        "percentile p not array",
        _g(p={"$percentile": {"input": "$x", "p": 0.5, "method": "approximate"}}),
    ),
    (
        "percentile p empty",
        _g(p={"$percentile": {"input": "$x", "p": [], "method": "approximate"}}),
    ),
    (
        "percentile p over one",
        _g(p={"$percentile": {"input": "$x", "p": [1.5], "method": "approximate"}}),
    ),
    (
        "percentile p negative",
        _g(p={"$percentile": {"input": "$x", "p": [-0.1], "method": "approximate"}}),
    ),
    (
        "percentile p string",
        _g(p={"$percentile": {"input": "$x", "p": ["a"], "method": "approximate"}}),
    ),
    (
        "percentile p expression",
        _g(p={"$percentile": {"input": "$x", "p": {"$literal": [0.5]}, "method": "approximate"}}),
    ),
    (
        "percentile p field path",
        _g(p={"$percentile": {"input": "$x", "p": "$x", "method": "approximate"}}),
    ),
    (
        "percentile p decimal",
        _g(p={"$percentile": {"input": "$x", "p": [Decimal128("0.5")], "method": "approximate"}}),
        _vals(INTS),
    ),
    (
        "percentile p nan",
        _g(p={"$percentile": {"input": "$x", "p": [NAN], "method": "approximate"}}),
    ),
    (
        "percentile input array",
        _g(p={"$percentile": {"input": "$x", "p": [0.5], "method": "approximate"}}),
        _vals([[1, 2], 3]),
    ),
    # --- $top / $bottom / N-accumulators ------------------------------------------------
    ("top", _gg(t={"$top": {"output": "$v", "sortBy": {"_id": -1}}})),
    ("bottom", _gg(t={"$bottom": {"output": ["$v", "$n"], "sortBy": {"n": 1}}})),
    ("topN", _gg(t={"$topN": {"n": 2, "output": "$_id", "sortBy": {"n": -1}}})),
    ("bottomN", _gg(t={"$bottomN": {"n": 2, "output": "$_id", "sortBy": {"n": -1}}})),
    ("firstN", _gg(t={"$firstN": {"n": 2, "input": "$v"}})),
    ("lastN", _gg(t={"$lastN": {"n": 2, "input": "$v"}})),
    ("maxN", _gg(t={"$maxN": {"n": 2, "input": "$v"}})),
    ("minN", _gg(t={"$minN": {"n": 2, "input": "$v"}})),
    (
        "minN null missing",
        _g(t={"$minN": {"n": 3, "input": "$x"}}),
        [{"_id": 1, "x": None}, {"_id": 2}, {"_id": 3, "x": 1}],
    ),
    (
        "firstN keeps null",
        _g(t={"$firstN": {"n": 3, "input": "$x"}}),
        [{"_id": 1, "x": None}, {"_id": 2}, {"_id": 3, "x": 1}],
    ),
    ("topN n long", _g(t={"$topN": {"n": Int64(2), "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN n double", _g(t={"$topN": {"n": 2.0, "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN n fraction", _g(t={"$topN": {"n": 2.5, "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN n zero", _g(t={"$topN": {"n": 0, "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN n negative", _g(t={"$topN": {"n": -1, "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN n string", _g(t={"$topN": {"n": "2", "output": "$_id", "sortBy": {"_id": 1}}})),
    (
        "topN n decimal",
        _g(t={"$topN": {"n": Decimal128("2"), "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    (
        "topN n expression",
        _gg(
            t={
                "$topN": {
                    "n": {"$cond": [{"$eq": ["$_id", "a"]}, 1, 2]},
                    "output": "$_id",
                    "sortBy": {"_id": 1},
                }
            }
        ),
    ),
    ("topN n field path", _g(t={"$topN": {"n": "$_id", "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN missing n", _g(t={"$topN": {"output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN missing sortBy", _g(t={"$topN": {"n": 1, "output": "$_id"}})),
    ("topN missing output", _g(t={"$topN": {"n": 1, "sortBy": {"_id": 1}}})),
    (
        "topN unknown field",
        _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {"_id": 1}, "z": 1}}),
    ),
    ("top with n", _g(t={"$top": {"n": 1, "output": "$_id", "sortBy": {"_id": 1}}})),
    ("topN sortBy not object", _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": 1}})),
    (
        "topN sortBy bad direction",
        _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {"_id": 2}}}),
    ),
    (
        "topN sortBy meta",
        _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {"s": {"$meta": "textScore"}}}}),
    ),
    ("firstN n large", _g(t={"$firstN": {"n": 1000, "input": "$_id"}})),
    ("firstN missing input", _g(t={"$firstN": {"n": 2}})),
    ("firstN not object", _g(t={"$firstN": 2})),
    ("maxN mixed", _g(t={"$maxN": {"n": 3, "input": "$x"}}), _vals(MIXED)),
    ("top missing output value", _g(t={"$top": {"output": "$nope", "sortBy": {"_id": 1}}})),
    ("topN ties", _g(t={"$topN": {"n": 2, "output": "$_id", "sortBy": {"g": 1}}})),
    # --- group keys -------------------------------------------------------------------
    (
        "key numbers collapse",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([1, 1.0, Int64(1), Decimal128("1.0"), 2]),
    ),
    (
        "key null and missing",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        [{"_id": 1, "x": None}, {"_id": 2}],
    ),
    (
        "key nan",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([NAN, Decimal128("NaN"), NAN]),
    ),
    (
        "key zero signs",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([0, -0.0, Decimal128("-0")]),
    ),
    (
        "key date oid",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([WHEN, WHEN, ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")]),
    ),
    (
        "key minkey maxkey",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([MinKey(), MaxKey(), MinKey()]),
    ),
    (
        "key timestamp",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Timestamp(1, 2), Timestamp(1, 2)]),
    ),
    (
        "key binary",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Binary(b"a", 0), Binary(b"a", 5), Binary(b"a", 0)]),
    ),
    (
        "key regex",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Regex("a", "i"), Regex("a", ""), Regex("a", "i")]),
    ),
    ("key code", [{"$group": {"_id": "$x", "c": {"$sum": 1}}}], _vals([Code("x"), Code("x"), "x"])),
    (
        "key bool and number",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([True, 1, False, 0]),
    ),
    ("key string case", [{"$group": {"_id": "$x", "c": {"$sum": 1}}}], _vals(["a", "A", "a"])),
    (
        "key arrays",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([[1, 2], [1, 2], [2, 1], [1.0, 2]]),
    ),
    (
        "key documents field order",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([{"a": 1, "b": 2}, {"b": 2, "a": 1}, {"a": 1.0, "b": 2}]),
    ),
    ("key compound", [{"$group": {"_id": {"g": "$g", "t": {"$type": "$n"}}, "c": {"$sum": 1}}}]),
    ("key compound missing", [{"$group": {"_id": {"a": "$nope", "g": "$g"}, "c": {"$sum": 1}}}]),
    (
        "key decimal",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Decimal128("1.50"), Decimal128("1.5"), 1.5]),
    ),
    (
        "key decimal precision",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Decimal128("0.1"), 0.1]),
    ),
    (
        "key large long and double",
        [{"$group": {"_id": "$x", "c": {"$sum": 1}}}],
        _vals([Int64(9007199254740993), 9007199254740992.0]),
    ),
    (
        "key undefined-ish",
        [{"$group": {"_id": "$x.y", "c": {"$sum": 1}}}],
        _vals([[{"y": 1}, {"y": 2}], {"y": 1}]),
    ),
    ("key constant", [{"$group": {"_id": 5, "c": {"$sum": 1}}}]),
    ("key expression error", [{"$group": {"_id": {"$ln": "$g"}, "c": {"$sum": 1}}}]),
    # --- $group parse errors ---------------------------------------------------------
    ("no _id", [{"$group": {"c": {"$sum": 1}}}]),
    ("unknown accumulator", _g(c={"$bogus": 1})),
    ("accumulator not object", _g(c=1)),
    ("two accumulators", _g(c={"$sum": 1, "$avg": 1})),
    ("empty accumulator", _g(c={})),
    ("field with dot", [{"$group": {"_id": None, "a.b": {"$sum": 1}}}]),
    ("field with dollar", [{"$group": {"_id": None, "$a": {"$sum": 1}}}]),
    ("group not object", [{"$group": 1}]),
    ("accumulator expression op", _g(c={"$add": [1, 2]})),
    ("_id operator object", [{"$group": {"_id": {"$sum": 1}}}]),
    ("_id two operators", [{"$group": {"_id": {"$a": 1, "$b": 2}}}]),
    (
        "accumulator js",
        _g(
            c={
                "$accumulator": {
                    "init": "function() {return 0}",
                    "accumulate": "function(s) {return s}",
                    "accumulateArgs": [],
                    "merge": "function(a, b) {return a}",
                    "lang": "js",
                }
            }
        ),
    ),
    ("concatArrays accumulator", _gg(c={"$concatArrays": "$arr"})),
    ("setUnion accumulator", _gg(c={"$setUnion": "$arr"})),
    # --- $sortByCount -----------------------------------------------------------------
    ("sortByCount", [{"$sortByCount": "$g"}]),
    (
        "sortByCount expression",
        [{"$sortByCount": {"$type": "$n"}}, {"$sort": {"count": -1, "_id": 1}}],
    ),
    ("sortByCount not expression", [{"$sortByCount": "g"}]),
    ("sortByCount object", [{"$sortByCount": {"a": 1}}]),
    ("sortByCount number", [{"$sortByCount": 1}]),
    ("sortByCount operator object", [{"$sortByCount": {"$a": 1, "$b": 2}}]),
    # --- $bucket ------------------------------------------------------------------------
    (
        "bucket",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10, 20], "default": "o"}}],
        _vals([1, 5, 10, 15, 25, "s"]),
    ),
    (
        "bucket output",
        [
            {
                "$bucket": {
                    "groupBy": "$x",
                    "boundaries": [0, 10, 20],
                    "default": "o",
                    "output": {"s": {"$sum": "$x"}, "p": {"$push": "$_id"}},
                }
            }
        ],
        _vals([1, 5, 10, 15, 25]),
    ),
    (
        "bucket no default outside",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10]}}],
        _vals([1, 15]),
    ),
    (
        "bucket mixed boundary types",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10.5, Int64(20)]}}],
        _vals([1, 15]),
    ),
    (
        "bucket string boundaries",
        [{"$bucket": {"groupBy": "$x", "boundaries": ["a", "m", "z"]}}],
        _vals(["b", "n"]),
    ),
    ("bucket unsorted", [{"$bucket": {"groupBy": "$x", "boundaries": [10, 0]}}], _vals([1])),
    ("bucket one boundary", [{"$bucket": {"groupBy": "$x", "boundaries": [0]}}], _vals([1])),
    (
        "bucket different types",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, "a"]}}],
        _vals([1]),
    ),
    (
        "bucket default in range",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10], "default": 5}}],
        _vals([1]),
    ),
    (
        "bucket default below",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10], "default": -1}}],
        _vals([1, 20]),
    ),
    (
        "bucket groupBy not expression",
        [{"$bucket": {"groupBy": "x", "boundaries": [0, 10]}}],
        _vals([1]),
    ),
    ("bucket missing groupBy", [{"$bucket": {"boundaries": [0, 10]}}], _vals([1])),
    (
        "bucket unknown field",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10], "z": 1}}],
        _vals([1]),
    ),
    (
        "bucket nan boundary",
        [{"$bucket": {"groupBy": "$x", "boundaries": [NAN, 0, 10]}}],
        _vals([1, NAN]),
    ),
    (
        "bucket decimal values",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 1, 2]}}],
        _vals([Decimal128("0.5"), Decimal128("1.0"), 1.5]),
    ),
    (
        "bucket output not accumulator",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 10], "output": {"a": 1}}}],
        _vals([1]),
    ),
    # --- $bucketAuto ------------------------------------------------------------------
    ("bucketAuto", [{"$bucketAuto": {"groupBy": "$x", "buckets": 3}}], _vals(list(range(10)))),
    (
        "bucketAuto output",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "output": {"a": {"$avg": "$x"}}}}],
        _vals(list(range(10))),
    ),
    (
        "bucketAuto more buckets than docs",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 20}}],
        _vals([1, 2, 3]),
    ),
    ("bucketAuto ties", [{"$bucketAuto": {"groupBy": "$x", "buckets": 2}}], _vals([1, 1, 1, 1, 2])),
    (
        "bucketAuto mixed types",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 3}}],
        _vals([1, "a", None, 2.5, [1]]),
    ),
    (
        "bucketAuto missing values",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2}}],
        [{"_id": 1}, {"_id": 2, "x": 1}],
    ),
    ("bucketAuto buckets zero", [{"$bucketAuto": {"groupBy": "$x", "buckets": 0}}], _vals([1])),
    (
        "bucketAuto buckets double",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2.0}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto buckets fraction",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2.5}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto buckets string",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": "2"}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto bad granularity",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R7"}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto granularity not string",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": 5}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto granularity non-numeric",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R5"}}],
        _vals([1, "a"]),
    ),
    (
        "bucketAuto granularity negative",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R5"}}],
        _vals([1, -2]),
    ),
    (
        "bucketAuto granularity nan",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "R5"}}],
        _vals([1, NAN]),
    ),
    (
        "bucketAuto granularity zero",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "granularity": "1-2-5"}}],
        _vals([0, 3, 70]),
    ),
    ("bucketAuto missing buckets", [{"$bucketAuto": {"groupBy": "$x"}}], _vals([1])),
    (
        "bucketAuto unknown field",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "z": 1}}],
        _vals([1]),
    ),
]


#: The same mistakes where mongod runs the `$group` in its CLASSIC engine (a
#: stage it cannot push down comes first): those codes are the ones this server
#: gives. Where the `$group` comes first mongod runs it in SBE, whose codes
#: differ (RM-280).
_CLASSIC = [{"$addFields": {"zz": 1}}]
CASES += [
    ("classic mergeObjects not object", _CLASSIC + _gg(m={"$mergeObjects": "$o"})),
    (
        "classic topN n zero",
        _CLASSIC + _g(t={"$topN": {"n": 0, "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    (
        "classic topN n fraction",
        _CLASSIC + _g(t={"$topN": {"n": 2.5, "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    (
        "classic topN n string",
        _CLASSIC + _g(t={"$topN": {"n": "2", "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    (
        "classic topN n null",
        _CLASSIC + _g(t={"$topN": {"n": None, "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    (
        "classic topN n field path",
        _CLASSIC + _g(t={"$topN": {"n": "$_id", "output": "$_id", "sortBy": {"_id": 1}}}),
    ),
    ("classic firstN n doc field", _CLASSIC + _gg(t={"$firstN": {"n": "$n", "input": "$v"}})),
    (
        "classic firstN n decimal",
        _CLASSIC + _gg(t={"$firstN": {"n": Decimal128("2"), "input": "$v"}}),
    ),
    ("classic firstN n nan", _CLASSIC + _gg(t={"$firstN": {"n": NAN, "input": "$v"}})),
    ("classic key expression error", _CLASSIC + [{"$group": {"_id": {"$ln": "$g"}}}]),
    ("classic concatArrays null", _CLASSIC + _g(c={"$concatArrays": "$x"}), _vals([[1], None])),
    ("classic setUnion not array", _CLASSIC + _g(c={"$setUnion": "$x"}), _vals([[1], 5])),
    (
        "n cond on missing _id",
        _gg(t={"$firstN": {"n": {"$cond": [{"$eq": ["$_id", "a"]}, 1, 2]}, "input": "$_id"}}),
    ),
    (
        "concatArrays missing skipped",
        _g(c={"$concatArrays": "$x"}),
        [{"_id": 1, "x": [1]}, {"_id": 2}, {"_id": 3, "x": [[2]]}],
    ),
    ("setUnion numbers", _g(c={"$setUnion": "$x"}), _vals([[1], [1.0, 2]])),
    ("group empty collection bad accumulator", _g(c={"$bogus": 1}), []),
    (
        "bucket empty collection bad output",
        [{"$bucket": {"groupBy": "$x", "boundaries": [0, 1], "output": {"a": 1}}}],
        [],
    ),
    (
        "bucketAuto empty collection bad output",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 1, "output": {"a": {"$count": 1}}}}],
        [],
    ),
    ("bucketAuto not object", [{"$bucketAuto": 1}], _vals([1])),
    ("bucketAuto groupBy not path", [{"$bucketAuto": {"groupBy": "x", "buckets": 2}}], _vals([1])),
    ("bucketAuto buckets huge", [{"$bucketAuto": {"groupBy": "$x", "buckets": 1e10}}], _vals([1])),
    (
        "bucketAuto buckets decimal",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": Decimal128("2")}}],
        _vals([1, 2]),
    ),
    (
        "bucketAuto buckets negative",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": -1}}],
        _vals([1]),
    ),
    (
        "bucketAuto output not object",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2, "output": 1}}],
        _vals([1]),
    ),
    (
        "bucketAuto 8 into 3",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 3}}],
        _vals(list(range(8))),
    ),
    (
        "bucketAuto 11 into 4",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 4}}],
        _vals(list(range(11))),
    ),
    (
        "bucketAuto 5 into 2",
        [{"$bucketAuto": {"groupBy": "$x", "buckets": 2}}],
        _vals(list(range(5))),
    ),
    ("group _id nested two operators", [{"$group": {"_id": {"k": {"$add": [1], "$b": 2}}}}]),
    ("accumulator argument two operators", _g(s={"$sum": {"$add": [1], "$b": 2}})),
    ("topN sortBy empty", _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {}}})),
    (
        "topN sortBy string direction",
        _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {"_id": "a"}}}),
    ),
    ("minN not object", _g(t={"$minN": 2})),
    ("bucketAuto buckets bool", [{"$bucketAuto": {"groupBy": "$x", "buckets": True}}], _vals([1])),
    ("bucketAuto buckets null", [{"$bucketAuto": {"groupBy": "$x", "buckets": None}}], _vals([1])),
    (
        "topN sortBy bool direction",
        _g(t={"$topN": {"n": 1, "output": "$_id", "sortBy": {"_id": True}}}),
    ),
    ("minN unknown field", _g(t={"$minN": {"n": 1, "input": "$x", "z": 1}})),
    ("minN missing n", _g(t={"$minN": {"input": "$x"}})),
]

#: Every preferred-number series over the same spread of values, so each
#: table and each rounding direction runs.
_SPREAD = [0.03, 0.7, 1.5, 3, 8.1, 42, 99.5, 250, 1234.5, Int64(98765), Decimal128("3.3")]
for _g_name in (
    "R5",
    "R10",
    "R20",
    "R40",
    "R80",
    "1-2-5",
    "E6",
    "E12",
    "E24",
    "E48",
    "E96",
    "E192",
    "POWERSOF2",
):
    for _n in (2, 4, 11):
        CASES.append(
            (
                f"bucketAuto {_g_name} {_n}",
                [{"$bucketAuto": {"groupBy": "$x", "buckets": _n, "granularity": _g_name}}],
                _vals(_SPREAD),
            )
        )

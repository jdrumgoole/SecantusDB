"""Parity: Rust `_secantus_core.apply_pipeline` vs pure-Python `apply_pipeline`.

Phase 2 net for the aggregation pipeline. For each (docs, pipeline) the Rust
path runs over BSON bytes; when it returns a concrete result (didn't defer), the
decoded list of docs must equal the authoritative pure-Python pipeline. When it
returns None (whole-pipeline fallback — an unported stage or a deferred inner
expression) there's nothing to assert.

Import-light: prefers the real `secantus.aggregate`, else loads `aggregate.py`
plus its pure deps by path under a stub `secantus`. The corpus uses only the
ported stages so the pure path never needs `secantus.storage`.
"""

from __future__ import annotations

import datetime as _dt
import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest
from bson.decimal128 import Decimal128

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_aggregate():
    try:
        from secantus import aggregate as a

        return a
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("paths", "collation", "query", "expressions", "ordering", "aggregate"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.aggregate"]


_pure = _load_pure_aggregate()
_PipelineContext = _pure.PipelineContext


def _rust_pipeline(docs, pipeline, vars=None, collation=None):
    res = _rust.apply_pipeline(
        bson.encode({"d": list(docs)}),
        bson.encode({"p": list(pipeline)}),
        bson.encode(vars or {}),
        bson.encode(collation or {}),
    )
    return None if res is None else bson.decode(res)["d"]


DOCS = [
    {"_id": 1, "a": 5, "b": "x", "tags": [1, 2]},
    {"_id": 2, "a": 15, "b": "y", "tags": [3]},
    {"_id": 3, "a": 25, "b": "z", "nested": {"k": 9}},
]

# A mixed-type corpus for $sort: numbers (int/float), strings, null, missing
# fields, nested docs/arrays — exercises order.cmp's cross-type ranks. (bool /
# NaN are intentionally excluded: they defer to Python — see order.is_sortable.)
SORT_DOCS = [
    {"_id": 1, "k": 3},
    {"_id": 2, "k": 1.5},
    {"_id": 3, "k": "apple"},
    {"_id": 5, "k": None},
    {"_id": 6},  # missing -> sorts as null
    {"_id": 7, "k": 1},
    {"_id": 8, "k": [1, 2]},
    {"_id": 9, "k": {"x": 1}},
    {"_id": 10, "k": "Apple"},
]

CURATED = [
    [{"$match": {"a": {"$gt": 10}}}],
    [{"$match": {"b": "y"}}],
    [{"$limit": 2}],
    [{"$skip": 1}],
    # $limit / $skip numeric-arg fidelity: a whole double computes on both
    # engines; bool / fractional / negative / (for $limit) zero are rejected —
    # Python raises the mongod code, the Rust core defers (skipped in this loop).
    [{"$limit": 2.0}],
    [{"$limit": 2.7}],
    [{"$limit": True}],
    [{"$limit": 0}],
    [{"$limit": -1}],
    [{"$skip": 3.0}],
    [{"$skip": 0}],
    [{"$skip": 3.7}],
    [{"$skip": True}],
    [{"$skip": -1}],
    [{"$count": "n"}],
    [{"$project": {"a": 1}}],
    [{"$project": {"a": 1, "_id": 0}}],
    [{"$project": {"b": 0}}],
    [{"$project": {"sum": {"$add": ["$a", 100]}, "_id": 0}}],
    # $getField reading an absent field resolves to the MISSING marker, which a
    # computed $project/$addFields field omits (Rust defers this to Python; both
    # sides must agree the field is dropped, not emitted null).
    [{"$project": {"r": {"$getField": {"field": "k", "input": "$nested"}}}}],
    [{"$addFields": {"r": {"$getField": {"field": "k", "input": "$nested"}}}}],
    [{"$project": {"r": "$$REMOVE"}}],
    [{"$addFields": {"c": {"$multiply": ["$a", 2]}}}],
    [{"$set": {"a": {"$add": ["$a", 1]}}}],
    [{"$unset": "b"}],
    [{"$unset": ["b", "tags"]}],
    [{"$replaceWith": {"x": "$a"}}],
    [{"$replaceRoot": {"newRoot": {"id": "$_id", "val": "$a"}}}],
    # chained
    [{"$match": {"a": {"$gte": 10}}}, {"$project": {"a": 1, "_id": 0}}],
    [{"$addFields": {"big": {"$gt": ["$a", 20]}}}, {"$match": {"big": True}}],
    [{"$skip": 1}, {"$limit": 1}, {"$count": "n"}],
    # $sort — single + multi-field, both directions (incl. whole-double ±1.0)
    [{"$sort": {"a": 1}}],
    [{"$sort": {"a": -1}}],
    [{"$sort": {"a": 1.0}}],
    [{"$sort": {"a": -1.0}}],
    [{"$sort": {"b": 1, "a": -1}}],
    # $unwind — string form, doc form, includeArrayIndex, preserve
    [{"$unwind": "$tags"}],
    [{"$unwind": {"path": "$tags", "includeArrayIndex": "ti"}}],
    [{"$unwind": {"path": "$tags", "preserveNullAndEmptyArrays": True}}],
    [{"$unwind": "$tags"}, {"$sort": {"tags": 1, "_id": 1}}],
    # $group — accumulators + key collision + chaining
    [{"$group": {"_id": "$b", "total": {"$sum": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": None, "n": {"$sum": 1}, "avg": {"$avg": "$a"}}}],
    [{"$group": {"_id": "$b", "mn": {"$min": "$a"}, "mx": {"$max": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "f": {"$first": "$a"}, "l": {"$last": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "all": {"$push": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "set": {"$addToSet": "$a"}}}, {"$sort": {"_id": 1}}],
    # $push / $addToSet skip a MISSING field value (nested is absent on some docs);
    # an all-missing field yields [] (not [null, ...]).
    [{"$group": {"_id": None, "p": {"$push": "$nested"}}}],
    [{"$group": {"_id": None, "s": {"$addToSet": "$nested"}}}],
    [{"$group": {"_id": None, "p": {"$push": "$nope"}}}],  # all missing -> []
    [{"$group": {"_id": None, "s": {"$addToSet": "$nope"}}}],
    # $mergeObjects accumulator — merge each operand doc across the group (later
    # keys win); null/missing operands skipped; an all-missing group yields {}.
    # `nested` is present on one doc and missing on the others (mixed shape).
    [{"$group": {"_id": None, "m": {"$mergeObjects": "$nested"}}}],
    [{"$group": {"_id": "$b", "m": {"$mergeObjects": "$nested"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": None, "m": {"$mergeObjects": "$nope"}}}],  # all missing -> {}
    # $median / $percentile — mongod's discrete percentile (probed 7.0.12):
    # sorted[max(0, ceil(p*n) - 1)] as a double; bool/NaN excluded; empty ->
    # null / per-p nulls. Both group-accumulator and expression forms.
    [
        {
            "$group": {
                "_id": None,
                "m": {"$median": {"input": "$a", "method": "approximate"}},
                "p": {
                    "$percentile": {
                        "input": "$a",
                        "p": [0.1, 0.25, 0.5, 0.75, 0.9, 1.0],
                        "method": "approximate",
                    }
                },
            }
        }
    ],
    [
        {"$group": {"_id": "$b", "m": {"$median": {"input": "$a", "method": "approximate"}}}},
        {"$sort": {"_id": 1}},
    ],
    [
        {
            "$project": {
                "m": {"$median": {"input": [3.5, 1, 2], "method": "approximate"}},
                "p": {"$percentile": {"input": [2, 4], "p": [0.5, 1.0], "method": "approximate"}},
            }
        }
    ],
    # $stdDevPop / $stdDevSamp — pop is 0 for a single value, samp is null for <2.
    [{"$group": {"_id": "$b", "sd": {"$stdDevPop": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "sd": {"$stdDevSamp": "$a"}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": None, "p": {"$stdDevPop": "$a"}, "s": {"$stdDevSamp": "$a"}}}],
    # $firstN / $lastN / $maxN / $minN accumulators — firstN/lastN keep nulls,
    # maxN/minN drop them; integral-double n accepted.
    [{"$group": {"_id": "$b", "r": {"$firstN": {"n": 2, "input": "$a"}}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "r": {"$lastN": {"n": 2, "input": "$a"}}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "r": {"$maxN": {"n": 2, "input": "$a"}}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": "$b", "r": {"$minN": {"n": 2, "input": "$a"}}}}, {"$sort": {"_id": 1}}],
    [{"$group": {"_id": None, "r": {"$firstN": {"n": 3, "input": "$a"}}}}],
    [{"$group": {"_id": None, "r": {"$maxN": {"n": 2.0, "input": "$a"}}}}],
    # $top / $bottom / $topN / $bottomN — sort by sortBy, take top/bottom output(s).
    [
        {"$group": {"_id": "$b", "r": {"$topN": {"n": 2, "sortBy": {"a": -1}, "output": "$a"}}}},
        {"$sort": {"_id": 1}},
    ],
    [
        {"$group": {"_id": "$b", "r": {"$bottomN": {"n": 2, "sortBy": {"a": 1}, "output": "$a"}}}},
        {"$sort": {"_id": 1}},
    ],
    [
        {"$group": {"_id": "$b", "r": {"$top": {"sortBy": {"a": -1}, "output": "$a"}}}},
        {"$sort": {"_id": 1}},
    ],
    [
        {"$group": {"_id": "$b", "r": {"$bottom": {"sortBy": {"a": 1}, "output": "$a"}}}},
        {"$sort": {"_id": 1}},
    ],
    [
        {
            "$group": {
                "_id": None,
                "r": {"$topN": {"n": 2, "sortBy": {"a": -1}, "output": ["$a", "$b"]}},
            }
        }
    ],
    [{"$group": {"_id": "$nested.k", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}],
    [{"$sortByCount": "$b"}],
    [{"$unwind": "$tags"}, {"$group": {"_id": "$tags", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}],
    # $bucket — default, custom output, empty buckets
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 10, 20, 30]}}],
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 10, 20], "default": "other"}}],
    # $bucket validation: invalid specs (out-of-range w/o default, unsorted,
    # missing groupBy, non-doc output) raise on Python and defer on Rust.
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 10]}}],  # 15/25 out of range
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 20, 10]}}],  # unsorted
    [{"$bucket": {"boundaries": [0, 30]}}],  # missing groupBy
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 30], "output": 5}}],  # non-doc output
    [
        {
            "$bucket": {
                "groupBy": "$a",
                "boundaries": [0, 20],
                "default": "x",
                "output": {"n": {"$sum": 1}, "av": {"$avg": "$a"}, "mx": {"$max": "$a"}},
            }
        }
    ],
    # $bucketAuto — count-chunking (equal values still split), custom output
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2}}],
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2.0}}],  # whole double accepted
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 3}}],
    [{"$sort": {"_id": 1}}, {"$bucketAuto": {"groupBy": "$_id", "buckets": 4}}],
    [
        {
            "$bucketAuto": {
                "groupBy": "$a",
                "buckets": 2,
                "output": {"n": {"$sum": 1}, "av": {"$avg": "$a"}},
            }
        }
    ],
    # $bucketAuto granularity — preferred-number rounding (Rust computes natively)
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": "R5"}}],
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": "R20"}}],
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 3, "granularity": "E6"}}],
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": "1-2-5"}}],
    [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": "POWERSOF2"}}],
    [
        {
            "$bucketAuto": {
                "groupBy": "$a",
                "buckets": 2,
                "granularity": "R10",
                "output": {"n": {"$sum": 1}, "mx": {"$max": "$a"}},
            }
        }
    ],
    # $redact — descend, pruning a nested sub-doc by content ($$PRUNE/$$DESCEND).
    [{"$redact": {"$cond": {"if": {"$eq": ["$k", 9]}, "then": "$$PRUNE", "else": "$$DESCEND"}}}],
    [{"$redact": "$$KEEP"}],
    # $facet — multiple sub-pipelines over the same input
    [
        {
            "$facet": {
                "byB": [{"$group": {"_id": "$b", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}],
                "top": [{"$sort": {"a": -1}}, {"$limit": 1}, {"$project": {"a": 1, "_id": 0}}],
                "n": [{"$count": "total"}],
            }
        }
    ],
    # $densify — numeric (handled) + date-unit (defers)
    [{"$densify": {"field": "a", "range": {"step": 5, "bounds": "full"}}}],
    [{"$densify": {"field": "a", "range": {"step": 10, "bounds": [0, 50]}}}],
    [{"$densify": {"field": "a", "range": {"step": 1, "unit": "day", "bounds": "full"}}}],
    # stages that still defer (rust None -> skipped)
    [{"$sample": {"size": 2}}],
]


def _densify_docs(rng):
    """Docs with an all-numeric densify field `v` (+ optional partition `g`)."""
    n = rng.randint(0, 5)
    docs = []
    for i in range(n):
        v = rng.choice([rng.randint(0, 30), float(rng.randint(0, 30)), rng.randint(0, 30) + 0.5])
        d = {"_id": i, "v": v}
        if rng.random() < 0.5:
            d["g"] = rng.choice(["p", "q"])
        docs.append(d)
    return docs


def _densify_spec(rng):
    step = rng.choice([1, 2, 5, 10, 2.5])
    rng_spec = {"step": step}
    if rng.random() < 0.5:
        lo = rng.randint(0, 10)
        rng_spec["bounds"] = [lo, lo + rng.choice([5, 10, 20])]
    else:
        rng_spec["bounds"] = "full"
    spec = {"field": "v", "range": rng_spec}
    if rng.random() < 0.4:
        spec["partitionByFields"] = ["g"]
    return {"$densify": spec}


def test_densify_fuzz():
    rng = random.Random(0xDE251F)
    handled = 0
    for _ in range(4000):
        docs = _densify_docs(rng)
        pipeline = [_densify_spec(rng)]
        docs = bson.decode(bson.encode({"d": docs}))["d"]
        pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
        rust = _rust_pipeline(docs, pipeline)
        if rust is None:
            continue
        try:
            py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
        except Exception:
            pytest.fail(f"rust={rust} but pure raised; pipeline={pipeline} docs={docs}")
        handled += 1
        assert rust == py, f"rust={rust} pure={py} pipeline={pipeline} docs={docs}"
    assert handled > 500, f"expected many handled densify pipelines, only {handled}"


def test_group_numeric_key_collision():
    # 1 (int), 1.0 (double), True must bucket together (first-seen _id wins),
    # and stay distinct from 2.
    docs = [
        {"_id": 1, "k": 1, "v": 10},
        {"_id": 2, "k": 1.0, "v": 5},
        {"_id": 3, "k": True, "v": 2},
        {"_id": 4, "k": 2, "v": 7},
    ]
    docs = bson.decode(bson.encode({"d": docs}))["d"]
    pipeline = bson.decode(
        bson.encode({"p": [{"$group": {"_id": "$k", "total": {"$sum": "$v"}, "n": {"$sum": 1}}}]})
    )["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, "expected the Rust $group to handle numeric-key collision"
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py}"


@pytest.mark.parametrize(
    "docs,pipeline",
    [
        # value fill (missing + null -> value), and value-as-expression.
        (
            [{"_id": 1, "a": 1}, {"_id": 2, "a": None}, {"_id": 3}],
            [{"$fill": {"output": {"a": {"value": 0}}}}],
        ),
        (
            [{"_id": 1, "a": 1, "b": 5}, {"_id": 2, "a": None, "b": 7}],
            [{"$fill": {"output": {"a": {"value": "$b"}}}}],
        ),
        # locf — leading null stays null, later gaps carry forward.
        (
            [
                {"_id": 1, "t": 1, "v": None},
                {"_id": 2, "t": 2, "v": 10},
                {"_id": 3, "t": 3},
                {"_id": 4, "t": 4, "v": 30},
                {"_id": 5, "t": 5},
            ],
            [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "locf"}}}}],
        ),
        # linear — numeric anchors with clean fractions; trailing null stays null.
        (
            [
                {"_id": 1, "t": 0, "v": 0},
                {"_id": 2, "t": 1},
                {"_id": 3, "t": 2},
                {"_id": 4, "t": 4, "v": 8},
                {"_id": 5, "t": 5},
            ],
            [{"$fill": {"sortBy": {"t": 1}, "output": {"v": {"method": "linear"}}}}],
        ),
        # partitionByFields + locf — output in partition-discovery order.
        (
            [
                {"_id": 1, "g": "a", "t": 1, "v": 10},
                {"_id": 2, "g": "b", "t": 1, "v": 5},
                {"_id": 3, "g": "a", "t": 2},
                {"_id": 4, "g": "b", "t": 2},
            ],
            [
                {
                    "$fill": {
                        "partitionByFields": ["g"],
                        "sortBy": {"t": 1},
                        "output": {"v": {"method": "locf"}},
                    }
                }
            ],
        ),
        # partitionBy expression.
        (
            [{"_id": 1, "g": "a", "t": 1, "v": 10}, {"_id": 2, "g": "a", "t": 2}],
            [
                {
                    "$fill": {
                        "partitionBy": "$g",
                        "sortBy": {"t": 1},
                        "output": {"v": {"method": "locf"}},
                    }
                }
            ],
        ),
    ],
)
def test_fill_parity(docs, pipeline):
    docs = bson.decode(bson.encode({"d": docs}))["d"]
    pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, f"expected Rust $fill to handle {pipeline}"
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py} pipeline={pipeline}"


@pytest.mark.parametrize(
    "docs,pipeline",
    [
        # Rank trio with a tie (sorted 10, 10, 20). $documentNumber 1/2/3;
        # $rank 1/1/3 (gap on tie); $denseRank 1/1/2 (no gap). Output stays in
        # input order.
        (
            [{"_id": 1, "s": 10}, {"_id": 2, "s": 20}, {"_id": 3, "s": 10}],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"s": 1},
                        "output": {
                            "dn": {"$documentNumber": {}},
                            "rk": {"$rank": {}},
                            "dr": {"$denseRank": {}},
                        },
                    }
                }
            ],
        ),
        # Running sum over ["unbounded", "current"] → 1, 3, 6.
        (
            [{"_id": 1, "t": 1, "v": 1}, {"_id": 2, "t": 2, "v": 2}, {"_id": 3, "t": 3, "v": 3}],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "run": {"$sum": "$v", "window": {"documents": ["unbounded", "current"]}}
                        },
                    }
                }
            ],
        ),
        # Sliding avg over [-1, 0] → 1.0, 1.5, 2.5 (clean doubles).
        (
            [{"_id": 1, "t": 1, "v": 1}, {"_id": 2, "t": 2, "v": 2}, {"_id": 3, "t": 3, "v": 3}],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"m": {"$avg": "$v", "window": {"documents": [-1, 0]}}},
                    }
                }
            ],
        ),
        # partitionBy expression + whole-partition (default) window sum: each row
        # gets its partition total (a:3, b:10). No sortBy.
        (
            [
                {"_id": 1, "g": "a", "v": 1},
                {"_id": 2, "g": "b", "v": 10},
                {"_id": 3, "g": "a", "v": 2},
            ],
            [{"$setWindowFields": {"partitionBy": "$g", "output": {"tot": {"$sum": "$v"}}}}],
        ),
        # $push over ["unbounded", "current"] within a partition.
        (
            [
                {"_id": 1, "g": "a", "t": 1, "v": "x"},
                {"_id": 2, "g": "a", "t": 2, "v": "y"},
                {"_id": 3, "g": "b", "t": 1, "v": "z"},
            ],
            [
                {
                    "$setWindowFields": {
                        "partitionBy": "$g",
                        "sortBy": {"t": 1},
                        "output": {
                            "hist": {
                                "$push": "$v",
                                "window": {"documents": ["unbounded", "current"]},
                            }
                        },
                    }
                }
            ],
        ),
        # $min/$max/$first/$last over the whole (sorted) partition.
        (
            [{"_id": 1, "t": 2, "v": 5}, {"_id": 2, "t": 1, "v": 3}, {"_id": 3, "t": 3, "v": 9}],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "mn": {"$min": "$v"},
                            "mx": {"$max": "$v"},
                            "f": {"$first": "$v"},
                            "l": {"$last": "$v"},
                        },
                    }
                }
            ],
        ),
        # Range windows: value-based bounds over a single ascending numeric sort.
        # Rolling [-1, 0] with a gap (no t=4), running [unbounded, current],
        # forward [current, unbounded], and a symmetric [-1, 1].
        (
            [
                {"_id": 1, "t": 1, "v": 10},
                {"_id": 2, "t": 2, "v": 20},
                {"_id": 3, "t": 3, "v": 30},
                {"_id": 4, "t": 5, "v": 50},
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"s": {"$sum": "$v", "window": {"range": [-1, 0]}}},
                    }
                }
            ],
        ),
        (
            [{"_id": i, "t": i, "v": (i + 1) * 10} for i in range(4)],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "r": {"$sum": "$v", "window": {"range": ["unbounded", "current"]}}
                        },
                    }
                }
            ],
        ),
        (
            [{"_id": i, "t": i, "v": (i + 1) * 10} for i in range(4)],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "f": {"$sum": "$v", "window": {"range": ["current", "unbounded"]}}
                        },
                    }
                }
            ],
        ),
        (
            [{"_id": 1, "t": 10, "v": 1}, {"_id": 2, "t": 11, "v": 2}, {"_id": 3, "t": 13, "v": 4}],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"a": {"$avg": "$v", "window": {"range": [-1, 1]}}},
                    }
                }
            ],
        ),
        # Date-unit range windows: a `unit` scales the offset and the x-axis is
        # the epoch-millis of a date sortBy. 2-day trailing sum, a `week` unit,
        # and a symmetric `hour` window all compute; a variable-length `month`
        # unit defers to Python.
        (
            [
                {
                    "_id": i,
                    "t": _dt.datetime(2020, 1, 1 + i, tzinfo=_dt.timezone.utc),
                    "v": (i + 1) * 10,
                }
                for i in range(5)
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "s": {"$sum": "$v", "window": {"range": [-2, 0], "unit": "day"}}
                        },
                    }
                }
            ],
        ),
        (
            [
                {"_id": i, "t": _dt.datetime(2020, 1, 1 + 7 * i, tzinfo=_dt.timezone.utc), "v": i}
                for i in range(4)
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "r": {"$sum": "$v", "window": {"range": [-1, 0], "unit": "week"}}
                        },
                    }
                }
            ],
        ),
        (
            [
                {
                    "_id": i,
                    "t": _dt.datetime(2020, 1, 1, i, tzinfo=_dt.timezone.utc),
                    "v": (i + 1) * 5,
                }
                for i in range(5)
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "a": {"$avg": "$v", "window": {"range": [-1, 1], "unit": "hour"}}
                        },
                    }
                }
            ],
        ),
        # $shift — value `by` positions away in sorted order: prev (default), next
        # (null out of range), self (by 0), and an expression output.
        (
            [{"_id": i, "t": i, "v": (i + 1) * 10} for i in range(4)],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "prev": {"$shift": {"output": "$v", "by": -1, "default": 0}},
                            "next": {"$shift": {"output": "$v", "by": 1}},
                            "self": {"$shift": {"output": "$v", "by": 0}},
                            "prev2": {"$shift": {"output": {"$add": ["$v", 1]}, "by": -2}},
                        },
                    }
                }
            ],
        ),
        # $expMovingAvg — N form (alpha = 2/(N+1)) and explicit alpha; IEEE-double
        # recurrence matches the oracle bit-for-bit. Also a per-partition case.
        (
            [{"_id": i, "t": i, "v": v} for i, v in enumerate([10, 20, 30, 40])],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "eN": {"$expMovingAvg": {"input": "$v", "N": 3}},
                            "eA": {"$expMovingAvg": {"input": "$v", "alpha": 0.4}},
                        },
                    }
                }
            ],
        ),
        (
            [
                {"_id": 1, "g": "a", "t": 1, "v": 2},
                {"_id": 2, "g": "a", "t": 2, "v": 4},
                {"_id": 3, "g": "b", "t": 1, "v": 100},
            ],
            [
                {
                    "$setWindowFields": {
                        "partitionBy": "$g",
                        "sortBy": {"t": 1},
                        "output": {"e": {"$expMovingAvg": {"input": "$v", "N": 2}}},
                    }
                }
            ],
        ),
        # $locf (carry forward; leading null stays null) + $linearFill (interpolate
        # on the t x-axis; trailing null stays null).
        (
            [
                {"_id": 1, "t": 0, "v": None},
                {"_id": 2, "t": 1, "v": 10},
                {"_id": 3, "t": 2, "v": None},
                {"_id": 4, "t": 4, "v": 40},
                {"_id": 5, "t": 5, "v": None},
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"lo": {"$locf": "$v"}, "li": {"$linearFill": "$v"}},
                    }
                }
            ],
        ),
        # $locf per-partition; carries a non-numeric value too.
        (
            [
                {"_id": 1, "g": "a", "t": 1, "v": "x"},
                {"_id": 2, "g": "a", "t": 2, "v": None},
                {"_id": 3, "g": "b", "t": 1, "v": None},
            ],
            [
                {
                    "$setWindowFields": {
                        "partitionBy": "$g",
                        "sortBy": {"t": 1},
                        "output": {"lo": {"$locf": "$v"}},
                    }
                }
            ],
        ),
        # $derivative / $integral — whole-partition (default window) slope & area,
        # plus a rolling 2-doc derivative. window is a sibling of the operator.
        (
            [
                {"_id": i, "t": t, "v": v}
                for i, (t, v) in enumerate([(0, 0), (1, 10), (2, 20), (4, 60)])
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "d": {"$derivative": {"input": "$v"}},
                            "i": {"$integral": {"input": "$v"}},
                            "rd": {
                                "$derivative": {"input": "$v"},
                                "window": {"documents": [-1, 0]},
                            },
                        },
                    }
                }
            ],
        ),
        # $derivative / $integral with a time `unit` over a date sortBy: the
        # x-axis is the date's epoch millis scaled into the unit, so the rate is
        # per hour. Both engines scale identically (millis / unit_ms in f64).
        (
            [
                {
                    "_id": i,
                    "t": _dt.datetime(2020, 1, 1, i, tzinfo=_dt.timezone.utc),
                    "v": v,
                }
                for i, v in enumerate([0, 10, 30, 45])
            ],
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "d": {"$derivative": {"input": "$v", "unit": "hour"}},
                            "i": {"$integral": {"input": "$v", "unit": "hour"}},
                        },
                    }
                }
            ],
        ),
    ],
)
def test_set_window_fields_parity(docs, pipeline):
    docs = bson.decode(bson.encode({"d": docs}))["d"]
    pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, f"expected Rust $setWindowFields to handle {pipeline}"
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py} pipeline={pipeline}"


@pytest.mark.parametrize(
    "pipeline",
    [
        # Time `unit` on a *numeric* sortBy — mongod requires a date sortBy, so
        # both sides reject it (Rust defers, Python raises).
        [
            {
                "$setWindowFields": {
                    "sortBy": {"t": 1},
                    "output": {"s": {"$sum": "$v", "window": {"range": [-1, 0], "unit": "day"}}},
                }
            }
        ],
        # Descending sort — range windows only support a single ascending field.
        [
            {
                "$setWindowFields": {
                    "sortBy": {"t": -1},
                    "output": {"s": {"$sum": "$v", "window": {"range": [-1, 0]}}},
                }
            }
        ],
    ],
)
def test_set_window_fields_range_unsupported_defers(pipeline):
    # These range shapes are mongod-valid but not ported → the Rust stage defers.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "t": 1, "v": 1}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
    assert _rust_pipeline(docs, pipeline) is None


@pytest.mark.parametrize("direction", [1, -1])
def test_sort_mixed_types(direction):
    docs = bson.decode(bson.encode({"d": SORT_DOCS}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$sort": {"k": direction, "_id": 1}}]}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, "expected the Rust $sort to handle the mixed-type corpus"
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py}"


@pytest.mark.parametrize(
    "acc",
    [
        {"$sum": "$k"},
        {"$avg": "$k"},
        {"$min": "$k"},
        {"$max": "$k"},
    ],
)
def test_group_accumulator_mixed_types(acc):
    # $sum/$avg ignore non-numeric; $min/$max order by BSON cross-type. The Rust
    # core computes these over the mixed corpus rather than deferring.
    docs = bson.decode(bson.encode({"d": SORT_DOCS}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$group": {"_id": None, "r": acc}}]}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, f"expected the Rust $group to handle {acc} over mixed types"
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py}"


@pytest.mark.parametrize("pipeline", CURATED)
def test_curated_parity(pipeline):
    docs = bson.decode(bson.encode({"d": DOCS}))["d"]
    pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    if rust is None:
        return
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py} pipeline={pipeline}"


_GRANULARITIES = [
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
]


def test_bucket_auto_granularity_fuzz():
    """Rust $bucketAuto granularity must equal pure-Python bit-for-bit (Python is
    itself pinned hex-exact to mongod 7.0.12 in test_crud). Broad random corpus
    over every series, bucket count, and decade scale."""
    rng = random.Random(0xB0CCE7)
    handled = 0
    for _ in range(400):
        gran = rng.choice(_GRANULARITIES)
        n = rng.randint(1, 40)
        scale = rng.choice([0.001, 0.1, 1, 4, 10, 100, 1000])
        vals = sorted(round(rng.uniform(0.5, 19) * scale, 6) for _ in range(n))
        docs = [
            {"_id": i, "a": (int(v) if rng.random() < 0.2 and v == int(v) else v)}
            for i, v in enumerate(vals)
        ]
        nb = rng.randint(1, 8)
        pipeline = [{"$bucketAuto": {"groupBy": "$a", "buckets": nb, "granularity": gran}}]
        docs_b = bson.decode(bson.encode({"d": docs}))["d"]
        pipeline_b = bson.decode(bson.encode({"p": pipeline}))["p"]
        rust = _rust_pipeline(docs_b, pipeline_b)
        assert rust is not None, f"Rust must handle granularity {gran}: {docs}"
        py = _pure.apply_pipeline(docs_b, pipeline_b, _PipelineContext())
        assert rust == py, f"gran={gran} rust={rust} pure={py} docs={docs} b={nb}"
        handled += 1
    assert handled == 400


@pytest.mark.parametrize(
    "values,code",
    [
        ([-5.0, 1.0, 2.0], 40260),  # negative -> non-negative only
        ([1.0, 2.0, "x"], 40258),  # non-numeric value
        ([float("nan"), 1.0], 40259),  # NaN
        ([None, 1.0, 2.0], 40258),  # null is non-numeric
        ([bson.Decimal128("1.5"), bson.Decimal128("2.5")], 2),  # Decimal128 deferral
    ],
)
def test_bucket_auto_granularity_value_defers_and_raises(values, code):
    docs = [{"_id": i, "a": v} for i, v in enumerate(values)]
    pipeline = [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": "R5"}}]
    docs_b = bson.decode(bson.encode({"d": docs}))["d"]
    pipeline_b = bson.decode(bson.encode({"p": pipeline}))["p"]
    assert _rust_pipeline(docs_b, pipeline_b) is None  # Rust defers
    with pytest.raises(Exception) as exc:
        _pure.apply_pipeline(docs_b, pipeline_b, _PipelineContext())
    assert getattr(exc.value, "code", None) == code


@pytest.mark.parametrize(
    "granularity,code",
    [
        ("R7", 40257),  # unknown series
        (5, 40261),  # non-string granularity
    ],
)
def test_bucket_auto_granularity_name_defers_and_raises(granularity, code):
    docs = [{"_id": 1, "a": 1.0}, {"_id": 2, "a": 2.0}]
    pipeline = [{"$bucketAuto": {"groupBy": "$a", "buckets": 2, "granularity": granularity}}]
    docs_b = bson.decode(bson.encode({"d": docs}))["d"]
    pipeline_b = bson.decode(bson.encode({"p": pipeline}))["p"]
    assert _rust_pipeline(docs_b, pipeline_b) is None  # Rust defers
    with pytest.raises(Exception) as exc:
        _pure.apply_pipeline(docs_b, pipeline_b, _PipelineContext())
    assert getattr(exc.value, "code", None) == code


@pytest.mark.parametrize(
    "spec,code",
    [
        ({"v": "asc"}, 15974),  # non-numeric direction
        ({"v": True}, 15974),  # bool direction
        ({"v": 0}, 15975),  # numeric non-±1
        ({"v": 2}, 15975),
        ({}, 15976),  # empty spec
    ],
)
def test_sort_stage_invalid_defers_and_raises(spec, code):
    # Invalid $sort stage: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "v": 1}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$sort": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


@pytest.mark.parametrize(
    "rng,code",
    [
        ({"step": 1, "unit": "day", "bounds": "full"}, 6053600),
        ({"step": True, "bounds": "full"}, 14),
        ({"step": 0, "bounds": "full"}, 5733401),
        ({"step": 1, "bounds": "partial"}, 5946802),
        ({"step": 1, "bounds": [0]}, 5733403),
        ({"step": 1, "bounds": [5, 0]}, 5733402),
    ],
)
def test_densify_invalid_defers_and_raises(rng, code):
    # Invalid $densify: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "v": 1}, {"_id": 2, "v": 5}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$densify": {"field": "v", "range": rng}}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


@pytest.mark.parametrize(
    "spec,code",
    [
        ({}, 40169),
        ({"a": 5}, 40170),
        ({"a": [5]}, 40171),
        ({"a": [{}]}, 40171),
        ({"a": [{"$facet": {"b": [{"$match": {"v": 1}}]}}]}, 40600),
    ],
)
def test_facet_invalid_defers_and_raises(spec, code):
    # Invalid $facet: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "v": 1}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$facet": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


@pytest.mark.parametrize(
    "spec,code",
    [
        ({"path": "a"}, 28818),  # bare path (no $)
        ({"path": 5}, 28808),  # non-string path
        ({"path": "$a", "includeArrayIndex": 5}, 28810),  # non-string index
        ({"path": "$a", "includeArrayIndex": ""}, 28810),  # empty index
        ({"path": "$a", "includeArrayIndex": "$i"}, 28822),  # $-prefixed index
        ({"path": "$a", "preserveNullAndEmptyArrays": 5}, 28809),  # non-bool preserve
        ("a", 28818),  # bare string form
    ],
)
def test_unwind_invalid_defers_and_raises(spec, code):
    # Invalid $unwind: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "a": [1, 2, 3]}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$unwind": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


@pytest.mark.parametrize(
    "spec,code",
    [
        (5, 40156),
        ("", 40157),
        ("$n", 40158),
        ("a.b", 40160),
        ("_id", 15948),
    ],
)
def test_count_invalid_defers_and_raises(spec, code):
    # Invalid $count: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1}, {"_id": 2}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$count": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


def test_project_empty_defers_and_raises():
    # Empty $project: Rust defers (None), pure engine raises Location51272.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "v": 1}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$project": {}}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == 51272


@pytest.mark.parametrize(
    "spec,code",
    [
        (5, 40149),
        (True, 40149),
        ([1], 40149),
        (None, 40149),
        ("v", 40148),
        ({"a": 1}, 40147),
    ],
)
def test_sort_by_count_invalid_defers_and_raises(spec, code):
    # Invalid $sortByCount: Rust defers (None), pure engine raises the mongod code.
    docs = bson.decode(bson.encode({"d": [{"_id": 1, "v": 1}]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$sortByCount": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


@pytest.mark.parametrize(
    "spec,code",
    [
        ({"groupBy": "$v", "buckets": True}, 40241),
        ({"groupBy": "$v", "buckets": "x"}, 40241),
        ({"groupBy": "$v", "buckets": 2.5}, 40242),
        ({"groupBy": "$v", "buckets": 0}, 40243),
        ({"groupBy": "$v", "buckets": -1}, 40243),
        ({"groupBy": "$v"}, 40246),
        ({"buckets": 2}, 40246),
    ],
)
def test_bucket_auto_invalid_defers_and_raises(spec, code):
    # Invalid $bucketAuto buckets: Rust defers (None), pure engine raises the code.
    docs = bson.decode(bson.encode({"d": [{"_id": i, "v": i} for i in range(6)]}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$bucketAuto": spec}]}))["p"]
    assert _rust_pipeline(docs, pipeline) is None
    with pytest.raises(_pure.AggregateError) as exc:
        _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert exc.value.code == code


def _rand_scalar(rng):
    r = rng.random()
    if r < 0.3:
        return rng.randint(0, 50)
    if r < 0.45:
        return rng.choice(["p", "q", "r"])
    if r < 0.6:
        return rng.choice([1.5, 2.0, 0.5])
    if r < 0.75:
        return rng.choice([True, False])
    if r < 0.85:
        return None
    return {"n": rng.randint(0, 9)}


def _rand_doc(rng):
    d = {"_id": rng.randint(1, 1000)}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.45:
            # array-valued (drives $unwind); may be empty
            d[f] = [_rand_scalar(rng) for _ in range(rng.randint(0, 3))]
        else:
            d[f] = _rand_scalar(rng)
    return d


def _rand_simple_stage(rng):
    """A non-recursive stage for $facet sub-pipelines (no $facet/$bucket)."""
    field = rng.choice(["a", "b", "c"])
    return rng.choice(
        [
            {"$match": {field: {rng.choice(["$gt", "$lt", "$eq"]): rng.randint(0, 50)}}},
            {"$limit": rng.randint(0, 3)},
            {"$skip": rng.randint(0, 2)},
            {"$count": "n"},
            {"$group": {"_id": "$" + field, "c": {"$sum": 1}}},
            {"$sort": {field: rng.choice([1, -1]), "_id": 1}},
            {"$sortByCount": "$" + field},
        ]
    )


def _rand_stage(rng):
    kind = rng.choice(
        [
            "match",
            "limit",
            "skip",
            "count",
            "project_in",
            "project_ex",
            "project_comp",
            "addfields",
            "unset",
            "replacewith",
            "sort",
            "sort_multi",
            "unwind",
            "unwind_idx",
            "group_count",
            "group_sum",
            "group_minmax",
            "group_push",
            "group_set",
            "group_std",
            "group_nelem",
            "group_topn",
            "sortbycount",
            "bucket",
            "bucket_default",
            "facet",
        ]
    )
    field = rng.choice(["a", "b", "c"])
    f2 = rng.choice(["a", "b", "c"])
    if kind == "match":
        op = rng.choice(["$gt", "$gte", "$lt", "$lte", "$eq", "$ne"])
        return {"$match": {field: {op: rng.randint(0, 50)}}}
    if kind == "limit":
        return {"$limit": rng.randint(0, 4)}
    if kind == "skip":
        return {"$skip": rng.randint(0, 4)}
    if kind == "count":
        return {"$count": "n"}
    if kind == "project_in":
        return {"$project": {field: 1}}
    if kind == "project_ex":
        return {"$project": {field: 0}}
    if kind == "project_comp":
        return {"$project": {"v": {"$add": ["$" + field, 1]}, "_id": 0}}
    if kind == "addfields":
        return {"$addFields": {"x": {"$multiply": ["$" + field, 2]}}}
    if kind == "unset":
        return {"$unset": field}
    if kind == "sort":
        return {"$sort": {field: rng.choice([1, -1]), "_id": 1}}
    if kind == "sort_multi":
        f2 = rng.choice(["a", "b", "c"])
        return {"$sort": {field: rng.choice([1, -1]), f2: rng.choice([1, -1]), "_id": 1}}
    if kind == "unwind":
        return {"$unwind": "$" + field}
    if kind == "unwind_idx":
        return {
            "$unwind": {
                "path": "$" + field,
                "includeArrayIndex": "idx",
                "preserveNullAndEmptyArrays": rng.choice([True, False]),
            }
        }
    if kind == "group_count":
        return {"$group": {"_id": "$" + field, "c": {"$sum": 1}}}
    if kind == "group_sum":
        return {"$group": {"_id": "$" + field, "s": {"$sum": "$" + f2}, "n": {"$sum": 1}}}
    if kind == "group_minmax":
        return {"$group": {"_id": "$" + field, "mn": {"$min": "$" + f2}, "mx": {"$max": "$" + f2}}}
    if kind == "group_push":
        return {"$group": {"_id": "$" + field, "p": {"$push": "$" + f2}, "av": {"$avg": "$" + f2}}}
    if kind == "group_set":
        return {"$group": {"_id": "$" + field, "set": {"$addToSet": "$" + f2}}}
    if kind == "group_std":
        op = rng.choice(["$stdDevPop", "$stdDevSamp"])
        return {"$group": {"_id": "$" + field, "sd": {op: "$" + f2}}}
    if kind == "group_nelem":
        op = rng.choice(["$firstN", "$lastN", "$maxN", "$minN"])
        spec = {"n": rng.randint(1, 3), "input": "$" + f2}
        return {"$group": {"_id": "$" + field, "r": {op: spec}}}
    if kind == "group_topn":
        sort_field = rng.choice(["a", "b", "c"])
        sort_by = {sort_field: rng.choice([1, -1])}
        op = rng.choice(["$top", "$bottom", "$topN", "$bottomN"])
        spec = {"sortBy": sort_by, "output": "$" + f2}
        if op in ("$topN", "$bottomN"):
            spec["n"] = rng.randint(1, 3)
        return {"$group": {"_id": "$" + field, "r": {op: spec}}}
    if kind == "sortbycount":
        return {"$sortByCount": "$" + field}
    if kind in ("bucket", "bucket_default"):
        # sorted, distinct numeric boundaries
        cuts = sorted(rng.sample([0, 5, 10, 15, 20, 30, 50], rng.randint(2, 4)))
        b = {"groupBy": "$" + field, "boundaries": cuts}
        if kind == "bucket_default":
            b["default"] = "other"
            b["output"] = {"n": {"$sum": 1}, "av": {"$avg": "$" + f2}}
        return {"$bucket": b}
    if kind == "facet":
        return {
            "$facet": {
                "x": [_rand_simple_stage(rng) for _ in range(rng.randint(1, 2))],
                "y": [_rand_simple_stage(rng) for _ in range(rng.randint(0, 2))],
            }
        }
    return {"$replaceWith": {"only": "$" + field}}


@pytest.mark.parametrize("seed", [0xA66E, 1, 2, 0xBEEF, 0xC0FFEE])
def test_pipeline_fuzz(seed):
    rng = random.Random(seed)
    handled = 0
    for _ in range(4000):
        docs = [_rand_doc(rng) for _ in range(rng.randint(0, 5))]
        pipeline = [_rand_stage(rng) for _ in range(rng.randint(1, 4))]
        docs = bson.decode(bson.encode({"d": docs}))["d"]
        pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
        rust = _rust_pipeline(docs, pipeline)
        if rust is None:
            continue
        try:
            py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
        except Exception:
            pytest.fail(f"rust={rust} but pure raised; pipeline={pipeline} docs={docs}")
        handled += 1
        assert rust == py, f"rust={rust} pure={py} pipeline={pipeline} docs={docs}"
    assert handled > 1000, f"expected many handled pipelines, only {handled}"


# `$sum` / `$avg` over Decimal128 compute natively in Rust now (they used to
# defer). Note these accumulators convert a double by its **exact** binary
# value, unlike `$inc`/`$mul`, which use 15 significant digits — mongod really
# does differ between the two, so the split is pinned on both sides.
DECIMAL_ACC_DOCS = [
    [{"_id": 1, "x": Decimal128("1.000000000000000000000000000000001")}, {"_id": 2, "x": 1}],
    [{"_id": 1, "x": Decimal128("2.50")}, {"_id": 2, "x": Decimal128("0.10")}],
    [{"_id": 1, "x": Decimal128("0")}, {"_id": 2, "x": 0.1}],
    [{"_id": 1, "x": Decimal128("0")}, {"_id": 2, "x": 1e10}],
    [{"_id": 1, "x": Decimal128("1.5")}, {"_id": 2, "x": 3.0}],
    [{"_id": 1, "x": Decimal128("-2.5")}, {"_id": 2, "x": Decimal128("2.5")}],
    [{"_id": 1, "x": Decimal128("1")}, {"_id": 2, "x": Decimal128("2")}, {"_id": 3, "x": 3}],
    [{"_id": 1, "x": Decimal128("1")}, {"_id": 2, "x": "skip"}, {"_id": 3, "x": None}],
]


@pytest.mark.parametrize("acc", ["$sum", "$avg"])
@pytest.mark.parametrize("docs", DECIMAL_ACC_DOCS, ids=range(len(DECIMAL_ACC_DOCS)))
def test_decimal_accumulators_match_pure_python(docs, acc):
    docs = [bson.decode(bson.encode(d)) for d in docs]
    pipeline = [{"$group": {"_id": None, "r": {acc: "$x"}}}]
    got = _rust_pipeline(docs, pipeline)
    assert got is not None, "Rust deferred; decimal accumulation should be native"
    want = _pure.apply_pipeline([dict(d) for d in docs], pipeline, _PipelineContext())
    assert got == want

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

import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest

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
    [{"$count": "n"}],
    [{"$project": {"a": 1}}],
    [{"$project": {"a": 1, "_id": 0}}],
    [{"$project": {"b": 0}}],
    [{"$project": {"sum": {"$add": ["$a", 100]}, "_id": 0}}],
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
    # $sort — single + multi-field, both directions
    [{"$sort": {"a": 1}}],
    [{"$sort": {"a": -1}}],
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
    [{"$group": {"_id": "$nested.k", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}],
    [{"$sortByCount": "$b"}],
    [{"$unwind": "$tags"}, {"$group": {"_id": "$tags", "c": {"$sum": 1}}}, {"$sort": {"_id": 1}}],
    # $bucket — default, custom output, empty buckets
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 10, 20, 30]}}],
    [{"$bucket": {"groupBy": "$a", "boundaries": [0, 10, 20], "default": "other"}}],
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


@pytest.mark.parametrize("direction", [1, -1])
def test_sort_mixed_types(direction):
    docs = bson.decode(bson.encode({"d": SORT_DOCS}))["d"]
    pipeline = bson.decode(bson.encode({"p": [{"$sort": {"k": direction, "_id": 1}}]}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    assert rust is not None, "expected the Rust $sort to handle the mixed-type corpus"
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

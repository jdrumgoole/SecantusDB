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
    for name in ("paths", "collation", "query", "expressions", "aggregate"):
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
    # stages that defer (rust None -> skipped)
    [{"$sort": {"a": 1}}],
    [{"$group": {"_id": "$b", "total": {"$sum": "$a"}}}],
    [{"$unwind": "$tags"}],
    [{"$sample": {"size": 2}}],
]


@pytest.mark.parametrize("pipeline", CURATED)
def test_curated_parity(pipeline):
    docs = bson.decode(bson.encode({"d": DOCS}))["d"]
    pipeline = bson.decode(bson.encode({"p": pipeline}))["p"]
    rust = _rust_pipeline(docs, pipeline)
    if rust is None:
        return
    py = _pure.apply_pipeline(docs, pipeline, _PipelineContext())
    assert rust == py, f"rust={rust} pure={py} pipeline={pipeline}"


def _rand_doc(rng):
    d = {"_id": rng.randint(1, 1000)}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.4:
            d[f] = rng.choice(["p", "q", "r"])
        elif r < 0.5:
            d[f] = {"n": rng.randint(0, 9)}
        else:
            d[f] = rng.randint(0, 50)
    return d


def _rand_stage(rng):
    kind = rng.choice(
        ["match", "limit", "skip", "count", "project_in", "project_ex",
         "project_comp", "addfields", "unset", "replacewith"]
    )
    field = rng.choice(["a", "b", "c"])
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
    return {"$replaceWith": {"only": "$" + field}}


def test_pipeline_fuzz():
    rng = random.Random(0xA66E)
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

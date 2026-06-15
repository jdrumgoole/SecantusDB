"""Parity: Rust `_secantus_core.apply_projection` vs pure-Python `apply_projection`.

Phase 1 net for the fifth ported leaf engine. For each (doc, spec) the Rust path
runs over BSON bytes; when it returns a concrete result (didn't defer), the
decoded value must equal the authoritative pure-Python `apply_projection`. When
it returns None (fallback — mixed inclusion/exclusion, nested-doc specs, unusual
$slice args, a deferred $elemMatch sub-filter) there's nothing to assert.

Import-light: prefers the real `secantus.projection`, else loads `projection.py`
+ its `paths` / `collation` / `query` deps by path under a stub `secantus`.
"""

from __future__ import annotations

import importlib.util
import pathlib
import random
import sys
import types

import bson
import pytest
from bson import ObjectId

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")


def _load_pure_projection():
    try:
        from secantus import projection as p

        return p
    except Exception:
        pass
    root = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
    if "secantus" not in sys.modules:
        pkg = types.ModuleType("secantus")
        pkg.__path__ = [str(root)]
        sys.modules["secantus"] = pkg
    for name in ("paths", "collation", "query", "projection"):
        full = f"secantus.{name}"
        if full not in sys.modules:
            spec = importlib.util.spec_from_file_location(full, root / f"{name}.py")
            mod = importlib.util.module_from_spec(spec)
            sys.modules[full] = mod
            spec.loader.exec_module(mod)
    return sys.modules["secantus.projection"]


_pure = _load_pure_projection()


def _rust_proj(doc, spec):
    res = _rust.apply_projection(bson.encode(doc), bson.encode(spec))
    return None if res is None else bson.decode(res)


CURATED = [
    ({"_id": 1, "a": 1, "b": 2, "c": 3}, {"a": 1, "c": 1}),
    ({"_id": 1, "a": 1, "b": 2}, {"a": 1, "_id": 0}),
    ({"_id": 1, "a": 1, "b": 2}, {"b": 0}),
    ({"_id": 1, "a": 1, "b": 2}, {"a": 0, "b": 0}),
    ({"_id": 1, "a": {"b": 2, "c": 3}}, {"a.b": 1}),
    ({"_id": 1, "a": {"b": 2, "c": 3}}, {"a.b": 0}),
    ({"_id": 1, "a": 1}, {"_id": 0, "a": 1}),
    ({"_id": 5, "x": 9}, {"_id": 0}),
    ({"_id": 5, "x": 9}, {"x": 0}),
    ({"_id": 1, "a": [1, 2, 3, 4, 5]}, {"a": {"$slice": 2}}),
    ({"_id": 1, "a": [1, 2, 3, 4, 5]}, {"a": {"$slice": -2}}),
    ({"_id": 1, "a": [1, 2, 3, 4, 5]}, {"a": {"$slice": [1, 2]}}),
    ({"_id": 1, "a": [1, 2, 3, 4, 5]}, {"a": {"$slice": [-3, 2]}}),
    # _id-only specs: truthy => inclusion (only _id); zero/False => drop _id.
    # Oracle-pinned against real mongod 2026-06-13: None and "" count as
    # include; only numeric zero / False mean drop.
    ({"_id": 1, "a": 1, "b": 2}, {"_id": 1}),
    ({"_id": 1, "a": 1, "b": 2}, {"_id": True}),
    ({"_id": 1, "a": 1, "b": 2}, {"_id": None}),
    ({"_id": 1, "a": 1, "b": 2}, {"_id": ""}),
    ({"_id": 1, "a": 1, "b": 2}, {"_id": 0.0}),
    ({"_id": 1, "a": 1, "b": 2}, {"_id": False}),
    # $slice with an explicit truthy _id flips to inclusion: only _id + the
    # sliced field survive (oracle-pinned).
    ({"_id": 1, "a": [1, 2, 3], "b": 9}, {"a": {"$slice": 2}, "_id": 1}),
    ({"_id": 1, "a": [1, 2, 3], "b": 9}, {"a": {"$slice": 2}, "_id": 0}),
    ({"_id": 1, "c": {"d": [1, 2, 3]}, "b": 9}, {"c.d": {"$slice": 1}, "_id": 1}),
    # Dotted paths through arrays + dict skeletons (oracle-pinned 2026-06-13):
    # inclusion maps over array elements (docs project, scalars drop),
    # exclusion unsets per element (scalars kept), missing leaves keep {}.
    ({"_id": 1, "a": [{"q": 1, "w": 2}, {"w": 3}, 7], "b": 9}, {"a.q": 1}),
    ({"_id": 1, "a": [{"q": 1, "w": 2}, {"w": 3}, 7], "b": 9}, {"a.q": 0}),
    ({"_id": 1, "a": [{"x": {"q": 1, "r": 2}}, {"x": 5}], "b": 9}, {"a.x.q": 1}),
    ({"_id": 1, "a": [{"q": 1, "w": 2, "z": 3}], "b": 9}, {"a.q": 1, "a.w": 1}),
    ({"_id": 1, "a": [[{"q": 1, "w": 2}], {"q": 5, "w": 6}]}, {"a.q": 1}),
    ({"_id": 1, "a": {"w": 2}, "b": 9}, {"a.q": 1}),
    ({"_id": 1, "a": 5, "b": 9}, {"a.q": 1}),
    ({"_id": 1, "a": [{"q": 1}], "b": 9}, {"a.0.q": 1}),
    ({"_id": 1, "a": [1, 2, 3], "b": 9}, {"a": {"$slice": 2}, "b": 1}),  # slice + inclusion
    (
        {"_id": 1, "items": [{"k": 1}, {"k": 5}, {"k": 9}]},
        {"items": {"$elemMatch": {"k": {"$gte": 5}}}},
    ),
    ({"_id": 1, "vals": [1, 5, 10]}, {"vals": {"$elemMatch": {"$gt": 4}}}),
    ({"_id": 1, "a": 1, "b": 2}, {}),  # empty spec
    ({"_id": 1, "a": 1, "b": 2}, {"a": True, "b": False}),  # mixed -> defer
    ({"a": 1, "b": 2}, {"a": 1}),  # no _id in doc
]


@pytest.mark.parametrize("doc,spec", CURATED)
def test_curated_parity(doc, spec):
    doc = bson.decode(bson.encode(doc))
    spec = bson.decode(bson.encode(spec))
    rust = _rust_proj(doc, spec)
    if rust is None:
        return
    py = _pure.apply_projection(doc, spec)
    assert rust == py, f"rust={rust} pure={py} spec={spec}"


def _rand_doc(rng):
    d = {"_id": rng.randint(1, 9)} if rng.random() < 0.8 else {}
    for f in ("a", "b", "c"):
        r = rng.random()
        if r < 0.2:
            continue
        elif r < 0.4:
            d[f] = [rng.randint(0, 9) for _ in range(rng.randint(0, 5))]
        elif r < 0.55:
            d[f] = {"x": rng.randint(0, 9), "y": rng.randint(0, 9)}
        else:
            d[f] = rng.choice([rng.randint(0, 9), "s", ObjectId()])
    return d


def _rand_spec(rng):
    kind = rng.choice(["incl", "excl", "slice", "incl_id0", "elem"])
    fields = rng.sample(["a", "b", "c", "a.x"], rng.randint(1, 2))
    if kind == "incl":
        return {f: 1 for f in fields}
    if kind == "excl":
        return {f: 0 for f in fields}
    if kind == "incl_id0":
        return {**{f: 1 for f in fields}, "_id": 0}
    if kind == "slice":
        n = rng.choice([1, 2, -1, -2, [1, 2], [-2, 1]])
        return {fields[0]: {"$slice": n}}
    return {fields[0]: {"$elemMatch": {"$gt": rng.randint(0, 9)}}}


def test_randomised_fuzz_parity():
    rng = random.Random(0x9809EC)
    handled = 0
    for _ in range(6000):
        doc = bson.decode(bson.encode(_rand_doc(rng)))
        spec = bson.decode(bson.encode(_rand_spec(rng)))
        rust = _rust_proj(doc, spec)
        if rust is None:
            continue
        handled += 1
        py = _pure.apply_projection(doc, spec)
        assert rust == py, f"divergence: rust={rust} pure={py} spec={spec} doc={doc}"
    assert handled > 1000, f"expected many handled cases, only {handled}"


def _rust_proj_batch(docs, spec):
    res = _rust.apply_projection_batch(bson.encode({"d": list(docs)}), bson.encode(spec))
    return None if res is None else bson.decode(res)["d"]


def test_batch_projection_parity():
    """The batched seam projects N docs, matching per-doc results, and defers the
    whole batch iff any single doc would defer."""
    assert _rust_proj_batch([], {"a": 1}) == []

    rng = random.Random(0x9809_BA7)
    handled = 0
    for _ in range(3000):
        docs = [bson.decode(bson.encode(_rand_doc(rng))) for _ in range(rng.randint(0, 6))]
        spec = bson.decode(bson.encode(_rand_spec(rng)))
        rust = _rust_proj_batch(docs, spec)
        per_doc = [_rust_proj(d, spec) for d in docs]
        if rust is None:
            assert any(r is None for r in per_doc) or not docs
            continue
        handled += 1
        py = [_pure.apply_projection(d, spec) for d in docs]
        assert rust == py, f"batch divergence: rust={rust} pure={py} spec={spec}"
    assert handled > 500, f"expected many handled batches, only {handled}"

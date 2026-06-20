"""Parity: Rust `_secantus_core.compute_update_description` vs pure-Python `diff`.

Phase 1 net for the sixth ported leaf engine (the change-stream `$v: 2` update
diff). For each (pre, post) the Rust path runs over BSON bytes; when it returns
a concrete result the decoded value must equal the authoritative pure-Python
`compute_update_description`. None means fallback (Decimal128 / exotic values).

`diff.py` has no intra-package imports, so it loads directly by path.
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

# Stub `secantus` package (with __path__) so diff.py's `from secantus import
# engine` auto-resolves without the server -> WiredTiger import chain.
_ROOT = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
if "secantus" not in sys.modules:
    _pkg = types.ModuleType("secantus")
    _pkg.__path__ = [str(_ROOT)]
    sys.modules["secantus"] = _pkg
_spec = importlib.util.spec_from_file_location("secantus_diff_pure", _ROOT / "diff.py")
_pure = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_pure)


def _rust_diff(pre, post):
    res = _rust.compute_update_description(bson.encode(pre), bson.encode(post))
    return None if res is None else bson.decode(res)


def _rust_apply(doc, diff):
    res = _rust.apply_update_description(bson.encode(doc), bson.encode(diff))
    return None if res is None else bson.decode(res)


CURATED = [
    ({"a": 1, "b": 2}, {"a": 9, "c": 3}),
    ({"a": {"b": 1, "c": 2}}, {"a": {"b": 1, "c": 9}}),
    ({"a": 1}, {"a": 1.0}),  # numeric bridge -> no change
    ({"a": [1, 2, 3, 4]}, {"a": [1, 9, 3]}),  # truncation + element change
    ({"a": [1, 2]}, {"a": [1, 2, 3]}),  # growth -> wholesale
    ({"a": [1, 2, 3]}, {"a": [1, 2, 3]}),  # identical
    ({"x": {"y": {"z": 1}}}, {"x": {"y": {"z": 2}}}),
    ({"a": 1, "b": 2, "c": 3}, {}),  # all removed
    ({}, {"a": 1, "b": 2}),  # all added
    ({"a": [{"k": 1}, {"k": 2}]}, {"a": [{"k": 1}, {"k": 9}]}),  # array of subdocs
    ({"a": True}, {"a": 1}),  # bool==int -> no change
    ({"a": "x"}, {"a": "y"}),
    # disambiguatedPaths: numeric-string dict keys (vs real array indices).
    ({"a": {"1": 1}}, {"a": {"1": 2}}),
    ({"a": [{"1": 1}]}, {"a": [{"1": 2}]}),
    ({"a": {"1": 1}, "b": 0}, {"b": 0}),  # removal under ambiguous path
    ({"a": {"2": [1, 2, 3]}}, {"a": {"2": [1]}}),  # truncation under ambiguous path
    (
        {"_id": ObjectId("0123456789abcdef01234567"), "n": 1},
        {"_id": ObjectId("0123456789abcdef01234567"), "n": 2},
    ),
]


@pytest.mark.parametrize("pre,post", CURATED)
def test_curated_parity(pre, post):
    pre = bson.decode(bson.encode(pre))
    post = bson.decode(bson.encode(post))
    rust = _rust_diff(pre, post)
    if rust is None:
        return
    py = _pure.compute_update_description(pre, post)
    assert rust == py, f"rust={rust} pure={py} pre={pre} post={post}"


def _rand_value(rng, depth):
    r = rng.random()
    if depth <= 0 or r < 0.5:
        return rng.choice(
            [
                rng.randint(0, 5),
                1.0,
                "s",
                "t",
                True,
                False,
                None,
                ObjectId("0123456789abcdef01234567"),
            ]
        )
    if r < 0.75:
        return [_rand_value(rng, depth - 1) for _ in range(rng.randint(0, 4))]
    return {k: _rand_value(rng, depth - 1) for k in rng.sample(["p", "q", "r"], rng.randint(0, 3))}


def _rand_doc(rng):
    return {k: _rand_value(rng, 2) for k in rng.sample(["a", "b", "c", "d"], rng.randint(0, 4))}


def test_randomised_fuzz_parity():
    rng = random.Random(0xD1FF)
    handled = 0
    for _ in range(6000):
        pre = bson.decode(bson.encode(_rand_doc(rng)))
        # Derive post by mutating pre sometimes, else an independent doc.
        post = pre if rng.random() < 0.1 else bson.decode(bson.encode(_rand_doc(rng)))
        rust = _rust_diff(pre, post)
        if rust is None:
            continue
        handled += 1
        py = _pure.compute_update_description(pre, post)
        assert rust == py, f"divergence: rust={rust} pure={py} pre={pre} post={post}"
    assert handled > 1000, f"expected many handled cases, only {handled}"


@pytest.mark.parametrize("pre,post", CURATED)
def test_apply_parity_curated(pre, post):
    """Rust `apply_update_description` matches pure Python: rolling the pre-image
    forward by the (pure-computed) diff produces the same document on both sides.
    This is the reverse of `compute`, the keystone of oplog replay."""
    pre = bson.decode(bson.encode(pre))
    post = bson.decode(bson.encode(post))
    diff = _pure.compute_update_description(pre, post)
    rust = _rust_apply(pre, diff)
    if rust is None:
        return  # fallback to pure Python
    py = _pure.apply_update_description(bson.decode(bson.encode(pre)), diff)
    assert rust == py, f"apply divergence: rust={rust} pure={py} diff={diff}"


def test_apply_parity_fuzz():
    """Same as the compute fuzz, but checks `apply` reproduces the pure-Python
    result across thousands of random pre/diff pairs."""
    rng = random.Random(0xA471)
    handled = 0
    for _ in range(6000):
        pre = bson.decode(bson.encode(_rand_doc(rng)))
        post = pre if rng.random() < 0.1 else bson.decode(bson.encode(_rand_doc(rng)))
        diff = _pure.compute_update_description(pre, post)
        rust = _rust_apply(pre, diff)
        if rust is None:
            continue
        handled += 1
        py = _pure.apply_update_description(bson.decode(bson.encode(pre)), diff)
        assert rust == py, f"apply divergence: rust={rust} pure={py} pre={pre} diff={diff}"
    assert handled > 1000, f"expected many handled cases, only {handled}"

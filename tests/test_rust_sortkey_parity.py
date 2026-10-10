"""Parity: Rust `_secantus_core` sortkey vs pure-Python `secantus.sortkey`.

This is the Phase 1 net for the first ported leaf engine
(tasks/rust-rewrite-plan.md). It asserts the Rust port produces byte-identical
sort keys to the authoritative pure-Python `encode_value` across a broad,
partly-randomised corpus — including the cross-type numeric collision that the
unified numeric index ordering depends on.

It is deliberately import-light: the pure-Python `sortkey` module is loaded by
file path so the test does not import the `secantus` package (which pulls in
the WiredTiger C extension), and the whole module skips cleanly when the Rust
extension hasn't been built. That lets it run both in full CI and in a
WiredTiger-less environment with just `pymongo` + the built wheel.
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
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex
from bson.timestamp import Timestamp

from parity_compare import same

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")

# Load the pure-Python encoder by path (avoid secantus/__init__ -> server ->
# WiredTiger import chain). A stub `secantus` package with __path__ lets
# sortkey.py's intra-package imports auto-resolve from src/secantus without the
# heavy server imports.
_ROOT = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus"
if "secantus" not in sys.modules:
    _pkg = types.ModuleType("secantus")
    _pkg.__path__ = [str(_ROOT)]
    sys.modules["secantus"] = _pkg
_spec = importlib.util.spec_from_file_location("secantus_sortkey_pure", _ROOT / "sortkey.py")
_pure = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_pure)

# Load collation.py too (for building Collation objects to pass to the pure
# encoder); the pure sortkey only imports it lazily when a collation is given.
_cspec = importlib.util.spec_from_file_location("secantus.collation", _ROOT / "collation.py")
_collation_mod = importlib.util.module_from_spec(_cspec)
sys.modules["secantus.collation"] = _collation_mod
_cspec.loader.exec_module(_collation_mod)
_Collation = _collation_mod.Collation


def _rust_encode(value, collation_wire=None):
    return _rust.sortkey_encode_value(bson.encode({"v": value}), bson.encode(collation_wire or {}))


def _rust_encode_directed(value, direction, collation_wire=None):
    return _rust.sortkey_encode_value_directed(
        bson.encode({"v": value}), direction, bson.encode(collation_wire or {})
    )


def _roundtrip(value):
    """The typed value as it survives a BSON round-trip — exactly what both
    encoders see, so int/int64/double/Decimal128 widths line up."""
    return bson.decode(bson.encode({"v": value}))["v"]


def _curated_values():
    tz = datetime.timezone(datetime.timedelta(hours=-8))
    return [
        None,
        MinKey(),
        MaxKey(),
        True,
        False,
        0,
        1,
        -1,
        1000,
        120,
        2**31 - 1,
        -(2**31),
        Int64(2**40),
        Int64(-(2**40)),
        1.5,
        -2.5,
        3.141592653589793,
        123.45,
        float("inf"),
        float("-inf"),
        float("nan"),
        Decimal128("1.00"),
        Decimal128("0"),
        Decimal128("123.45"),
        Decimal128("-1E-6"),
        Decimal128("Infinity"),
        Decimal128("NaN"),
        3,
        3.0,
        Decimal128("3"),
        "",
        "hello",
        "café\U0001f600",
        "a\x00b\x00c",
        ObjectId("0123456789abcdef01234567"),
        datetime.datetime(2026, 6, 5, 12, 0, 0, tzinfo=datetime.timezone.utc),
        datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc),
        datetime.datetime(1960, 1, 1, tzinfo=datetime.timezone.utc),
        datetime.datetime(2030, 3, 4, 5, 6, 7, 891000, tzinfo=tz),
        Timestamp(1700000000, 7),
        Binary(b"\x00\x01\xff\x00ab", 0),
        [1, 2, 3],
        ["a", "b"],
        [1, "two", 3.0, None],
        {"a": 1, "b": "x"},
        {"nested": {"deep": [1, {"k": ObjectId()}]}},
        Regex("^abc$", "im"),
        # bson.Code subclasses str, so both engines rank it RANK_STRING.
        Code("function(){}"),
        Code("x"),
    ]


@pytest.mark.parametrize("value", _curated_values())
def test_curated_value_parity(value):
    v = _roundtrip(value)
    assert same(_rust_encode(v), _pure.encode_value(v))


@pytest.mark.parametrize("value", _curated_values())
def test_curated_directed_parity(value):
    v = _roundtrip(value)
    for direction in (1, -1):
        assert same(_rust_encode_directed(v, direction), _pure.encode_value_directed(v, direction))


# COLLATED keys are deliberately NOT pinned to the Python encoder any more.
# Since 2026-10-10 the Rust engine encodes a string under a collation as its
# ICU4X sort key, which is what makes it order and match as mongod does
# (`tools/probes/collation.py`: 8 of 764 results differ from 8.2.11, where the
# previous fold shared with Python differed in 462). The Python encoder still
# writes its own folded string, so the two produce different bytes by design
# and neither is the other's reference. What this gives up: this suite no
# longer notices a change to either engine's collated encoding. The Rust side
# is covered by `crates/secantus-core/src/collation.rs` tests and the probe;
# the on-disk consequence is recorded in tasks/backlog.md 7.070.
#
# What can still be said without a reference is checked below: two strings
# the collation calls equal encode alike, and unequal ones do not.
@pytest.mark.parametrize(
    "a,b,strength,equal",
    [
        ("PING", "ping", 2, True),
        ("PING", "ping", 3, False),
        ("café", "CAFE", 1, True),
        ("café", "cafe", 2, False),
        ("Hello World", "hello world", 2, True),
    ],
)
def test_collated_keys_agree_exactly_when_the_collation_does(a, b, strength, equal):
    wire = {"locale": "en", "strength": strength}
    assert (_rust_encode(a, wire) == _rust_encode(b, wire)) is equal
    assert _rust_encode(a, wire) != _rust_encode(a)


@pytest.mark.parametrize("value", _curated_values())
def test_curated_id_key_parity(value):
    """The frozen `_id` key encoding (entry formats 1-3's documents) must match
    too: it is stored in every document row, so a drift strands documents."""
    v = _roundtrip(value)
    rust = _rust.sortkey_encode_id_key(bson.encode({"v": v}))
    assert same(rust, _pure.encode_id_key(v))


def test_nested_strings_take_the_collation():
    """An index's collation applies to strings INSIDE documents and arrays, as
    mongod's comparison does. (Not compared with the Python encoder: see the
    note above `test_collated_keys_agree_exactly_when_the_collation_does`.)"""
    wire = {"locale": "en", "strength": 2}
    upper = {"a": "PING", "b": ["X", {"c": "y"}]}
    lower = {"a": "ping", "b": ["x", {"c": "Y"}]}
    assert _rust_encode(upper, wire) == _rust_encode(lower, wire)
    assert _rust_encode(upper) != _rust_encode(lower)


def test_cross_type_numeric_collision_matches_python():
    # The headline property, asserted on both implementations at once.
    for triple in ([3, 3.0, Decimal128("3")], [1, Decimal128("1.00"), 1.0]):
        rust_keys = {bytes(_rust_encode(_roundtrip(x))) for x in triple}
        py_keys = {bytes(_pure.encode_value(_roundtrip(x))) for x in triple}
        assert len(rust_keys) == 1, "rust keys must collide on equal numeric value"
        assert same(rust_keys, py_keys)


def _random_value(rng: random.Random):
    kind = rng.choice(["int", "int64", "float", "dec", "str", "oid", "date", "bin", "arr"])
    if kind == "int":
        return rng.randint(-(2**31), 2**31 - 1)
    if kind == "int64":
        return Int64(rng.randint(-(2**52), 2**52))
    if kind == "float":
        return round(rng.uniform(-1e6, 1e6), rng.randint(0, 6))
    if kind == "dec":
        return Decimal128(str(round(rng.uniform(-1e5, 1e5), rng.randint(0, 4))))
    if kind == "str":
        n = rng.randint(0, 12)
        return "".join(rng.choice("ab🙂z \x00çé") for _ in range(n))
    if kind == "oid":
        return ObjectId()
    if kind == "date":
        return datetime.datetime.fromtimestamp(rng.uniform(0, 2e9), tz=datetime.timezone.utc)
    if kind == "bin":
        return Binary(bytes(rng.randrange(256) for _ in range(rng.randint(0, 10))), 0)
    return [rng.randint(-100, 100) for _ in range(rng.randint(0, 4))]


def test_randomised_fuzz_parity():
    rng = random.Random(20260605)
    for _ in range(2000):
        v = _roundtrip(_random_value(rng))
        rust = _rust_encode(v)
        py = _pure.encode_value(v)
        assert same(rust, py), f"divergence on {v!r}: rust={rust.hex()} py={py.hex()}"

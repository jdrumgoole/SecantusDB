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

import bson
import pytest
from bson import Binary, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex
from bson.timestamp import Timestamp

_rust = pytest.importorskip("_secantus_core", reason="Rust core extension not built")

# Load the pure-Python encoder directly by path (avoid secantus/__init__ ->
# server -> WiredTiger import chain; the encoder only needs bson).
_SORTKEY_PATH = pathlib.Path(__file__).resolve().parents[1] / "src" / "secantus" / "sortkey.py"
_spec = importlib.util.spec_from_file_location("secantus_sortkey_pure", _SORTKEY_PATH)
_pure = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_pure)


def _rust_encode(value):
    return _rust.sortkey_encode_value(bson.encode({"v": value}))


def _rust_encode_directed(value, direction):
    return _rust.sortkey_encode_value_directed(bson.encode({"v": value}), direction)


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
    ]


@pytest.mark.parametrize("value", _curated_values())
def test_curated_value_parity(value):
    v = _roundtrip(value)
    assert _rust_encode(v) == _pure.encode_value(v)


@pytest.mark.parametrize("value", _curated_values())
def test_curated_directed_parity(value):
    v = _roundtrip(value)
    for direction in (1, -1):
        assert _rust_encode_directed(v, direction) == _pure.encode_value_directed(v, direction)


def test_cross_type_numeric_collision_matches_python():
    # The headline property, asserted on both implementations at once.
    for triple in ([3, 3.0, Decimal128("3")], [1, Decimal128("1.00"), 1.0]):
        rust_keys = {bytes(_rust_encode(_roundtrip(x))) for x in triple}
        py_keys = {bytes(_pure.encode_value(_roundtrip(x))) for x in triple}
        assert len(rust_keys) == 1, "rust keys must collide on equal numeric value"
        assert rust_keys == py_keys


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
        assert rust == py, f"divergence on {v!r}: rust={rust.hex()} py={py.hex()}"

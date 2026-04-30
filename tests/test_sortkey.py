from __future__ import annotations

import datetime as _dt
import math
import random

import pytest
from bson import Binary, Decimal128, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus.sortkey import encode_compound, encode_value


def _sorted(values: list) -> list[bytes]:
    return [encode_value(v) for v in values]


def _is_increasing(seq: list[bytes]) -> bool:
    return all(seq[i] < seq[i + 1] for i in range(len(seq) - 1))


def test_cross_type_order() -> None:
    """Across types, MongoDB's BSON sort order is preserved by byte order."""
    values = [
        MinKey(),
        None,
        -math.inf,
        -1,
        0,
        1,
        math.inf,
        "",
        "abc",
        {"a": 1},
        [1, 2],
        Binary(b"abc"),
        ObjectId("000000000000000000000000"),
        False,  # bool sorts after ObjectId in BSON cross-type order
        True,
        _dt.datetime(2020, 1, 1),
        Timestamp(1, 0),
        Regex("x"),
        MaxKey(),
    ]
    encoded = _sorted(values)
    assert _is_increasing(encoded)


def test_numeric_equality_across_types() -> None:
    assert encode_value(5) == encode_value(5.0)
    assert encode_value(5) == encode_value(Decimal128("5"))
    assert encode_value(0) == encode_value(0.0)
    assert encode_value(0) == encode_value(Decimal128("0"))


def test_numeric_ordering_signed() -> None:
    # Mix of signs / magnitudes / decimals.
    values = [-1000, -100, -1.5, -1, -0.5, 0, 0.5, 1, 1.5, 100, 1000]
    encoded = _sorted(values)
    assert _is_increasing(encoded)


def test_numeric_ordering_decimal_precision() -> None:
    values = [
        Decimal128("0.1"),
        Decimal128("0.5"),
        Decimal128("1"),
        Decimal128("1.5"),
        Decimal128("99"),
        Decimal128("99.9"),
        Decimal128("100"),
        Decimal128("1000"),
    ]
    encoded = _sorted(values)
    assert _is_increasing(encoded)


def test_numeric_ordering_random_ints() -> None:
    rng = random.Random(0xBEEF)
    values = sorted(rng.randint(-(10**6), 10**6) for _ in range(200))
    encoded = _sorted(values)
    assert _is_increasing(encoded) or all(
        encoded[i] <= encoded[i + 1] for i in range(len(encoded) - 1)
    )


def test_nan_sorts_below_neg_infinity() -> None:
    assert encode_value(float("nan")) < encode_value(-math.inf)
    assert encode_value(-math.inf) < encode_value(0)
    assert encode_value(0) < encode_value(math.inf)


def test_string_ordering() -> None:
    values = ["", "a", "ab", "abc", "abd", "b", "ba"]
    assert _is_increasing(_sorted(values))


def test_string_with_null_byte_does_not_collide_with_separator() -> None:
    a = encode_value("foo")
    b = encode_value("foo\x00bar")
    c = encode_value("foobar")
    assert len({a, b, c}) == 3
    assert a < b < c or a < c < b  # exact order doesn't matter, just distinct


def test_bool_distinct_from_one() -> None:
    assert encode_value(True) != encode_value(1)
    assert encode_value(False) != encode_value(0)


def test_objectid_ordering() -> None:
    earliest = ObjectId("000000000000000000000000")
    middle = ObjectId("ffffffff00000000ff000000")
    latest = ObjectId("ffffffffffffffffffffffff")
    assert encode_value(earliest) < encode_value(middle) < encode_value(latest)


def test_date_ordering() -> None:
    a = _dt.datetime(1900, 1, 1)
    b = _dt.datetime(1970, 1, 1)
    c = _dt.datetime(2020, 6, 15, 12, 30, 45)
    d = _dt.datetime(2099, 12, 31)
    assert encode_value(a) < encode_value(b) < encode_value(c) < encode_value(d)


def test_timestamp_ordering() -> None:
    a = Timestamp(0, 0)
    b = Timestamp(1, 0)
    c = Timestamp(1, 5)
    d = Timestamp(2, 0)
    assert encode_value(a) < encode_value(b) < encode_value(c) < encode_value(d)


def test_compound_ordering() -> None:
    values = [
        ["a", 1],
        ["a", 2],
        ["a", 100],
        ["b", 0],
        ["b", 1],
    ]
    encoded = [encode_compound(v) for v in values]
    assert _is_increasing(encoded)


def test_compound_string_with_null_does_not_break_separator() -> None:
    a = encode_compound(["foo", 1])
    b = encode_compound(["foo\x00bar", 1])
    c = encode_compound(["foo", 2])
    # Each value should be distinct.
    assert len({a, b, c}) == 3
    # Compound first-component ordering: "foo" vs "foo\x00bar".
    # "foo\x00bar" sorts after "foo" because the escaped null is non-empty
    # additional bytes beyond the bare "foo" component.
    assert a < b


def test_compound_prefix_equality_bytes() -> None:
    """Two compound keys with the same first component share that prefix."""
    a = encode_compound(["alpha", 1])
    b = encode_compound(["alpha", 99])
    c = encode_compound(["beta", 1])
    # a and b share the leading component bytes; c diverges earlier.
    common = encode_value("alpha")
    assert a.startswith(common)
    assert b.startswith(common)
    assert not c.startswith(common)


@pytest.mark.parametrize(
    "v",
    [
        0,
        1,
        -1,
        100,
        -100,
        1.5,
        -1.5,
        Decimal128("3.14"),
        Decimal128("-2.71"),
        "abc",
        b"abc",
        Binary(b"abc"),
        ObjectId(),
        _dt.datetime(2020, 1, 1),
        True,
        False,
        None,
        MinKey(),
        MaxKey(),
        [1, 2, 3],
        {"k": 1},
    ],
)
def test_encode_value_is_deterministic(v: object) -> None:
    assert encode_value(v) == encode_value(v)

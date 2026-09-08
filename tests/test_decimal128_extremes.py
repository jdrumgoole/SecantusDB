"""Decimal128 values outside `f64`'s range, through the pure Python engine.

decimal128 spans `1E-6176` to `9.999…E+6144`; `f64` spans about `1E-308` to
`1E+308`. Wherever an operator asked its classifying question — *is this
infinite? is this zero? is this negative?* — of a `float()` rendering of the
argument, that rendering saturated and the operator took the wrong branch.

Measured against mongod 8.2.11 on 2026-09-07 over 14 operators × 26 inputs:
**64 of 364 cells diverged**, including three that raised a raw
`decimal.Inexact` / `decimal.Overflow` out of the evaluator — an internal
server error where mongod returns a value. Every expectation below is that
server's own answer, copied verbatim; the grid is now 0.

The Rust server's half of the same sweep is `test_rust_decimal_extremes.py`.
"""

from __future__ import annotations

import pytest
from bson import Decimal128

from secantus.expressions import ExpressionError, evaluate


def apply(op, value):
    return evaluate({op: {"$literal": Decimal128(value)}}, {})


# ---------------------------------------------------------------------------
# The saturating classifiers
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("value", ["-1E-6176", "-1E-6100", "-1E+6144", "-1"])
def test_sqrt_of_a_negative_decimal_is_a_domain_error(value):
    """`float(Decimal("-1E-6176"))` is `-0.0`, which is not `< 0`."""
    with pytest.raises(ExpressionError) as e:
        apply("$sqrt", value)
    assert e.value.code == 28714


@pytest.mark.parametrize("value,expected", [("-0", "-0"), ("-0.00", "-0.0")])
def test_sqrt_of_a_negative_zero_keeps_the_sign(value, expected):
    """A negative ZERO is in the domain; only a negative non-zero raises."""
    assert str(apply("$sqrt", value)) == expected


SQRT = [
    ("4", "2"),
    ("2.5", "1.581138830084189665999446772216359"),
    ("1E+6144", "1.00000000000000000E+3072"),
    ("1E-6176", "1E-3088"),
    ("9.999999999999999999999999999999999E+6144", "3.162277660168379331998893544432718E+3072"),
]


@pytest.mark.parametrize("value,expected", SQRT, ids=[v for v, _ in SQRT])
def test_sqrt_matches_mongod(value, expected):
    assert str(apply("$sqrt", value)) == expected


@pytest.mark.parametrize("value", ["1E-6176", "-1E-6176", "1E-400", "4.9E-324"])
def test_tiny_decimal_is_truthy(value):
    assert apply("$toBool", value) is True


# ---------------------------------------------------------------------------
# $toDouble converts only into the NORMAL range
# ---------------------------------------------------------------------------

TO_DOUBLE_241 = [
    "1E+6144",
    "1E+310",
    "-1E+6144",
    "1E-6176",
    "1E-400",
    "4.9E-324",  # representable as a SUBNORMAL double, and still a 241
    "2.2250738585072012E-308",
    # The tininess cut TRUNCATED to 34 digits: still below it.
    "2.225073858507201259573821257020768E-308",
]


@pytest.mark.parametrize("value", TO_DOUBLE_241)
def test_to_double_refuses_overflow_and_subnormal(value):
    with pytest.raises(ExpressionError) as e:
        apply("$toDouble", value)
    assert e.value.code == 241


@pytest.mark.parametrize(
    "value,expected",
    [
        ("0", 0.0),
        ("-0", -0.0),
        ("1", 1.0),
        ("2.2250738585072013E-308", 2.2250738585072014e-308),
    ],
)
def test_to_double_converts_normals_and_zero(value, expected):
    got = apply("$toDouble", value)
    assert got == expected and str(got) == str(expected)


# ---------------------------------------------------------------------------
# $floor / $ceil quantize (NaN past 34 digits); $trunc / $round widen instead
# ---------------------------------------------------------------------------

QUANTIZE = [
    ("1E+33", "1000000000000000000000000000000000"),
    ("1E+34", "NaN"),
    ("1E+6144", "NaN"),
    ("-1E+6144", "NaN"),
    ("1E+310", "NaN"),
]


@pytest.mark.parametrize("value,expected", QUANTIZE, ids=[v for v, _ in QUANTIZE])
def test_floor_and_ceil_quantize(value, expected):
    assert str(apply("$floor", value)) == expected
    assert str(apply("$ceil", value)) == expected


WIDEN = [
    ("1E+34", "1.000000000000000000000000000000000E+34"),
    ("1E+6144", "1.000000000000000000000000000000000E+6144"),
    ("1E+310", "1.000000000000000000000000000000000E+310"),
    ("9.999999999999999999999999999999999E+6144", "9.999999999999999999999999999999999E+6144"),
]


@pytest.mark.parametrize("value,expected", WIDEN, ids=[v for v, _ in WIDEN])
def test_trunc_and_round_widen_rather_than_fail(value, expected):
    """The asymmetry: `$floor` of these is `NaN`, `$trunc` is the value."""
    assert str(apply("$trunc", value)) == expected
    assert str(apply("$round", value)) == expected


# ---------------------------------------------------------------------------
# The angle conversions: one correctly-rounded multiply, into the subnormals
# ---------------------------------------------------------------------------

DEG_RAD = [
    ("$degreesToRadians", "1", "0.01745329251994329576923690768488613"),
    ("$degreesToRadians", "180", "3.141592653589793238462643383279503"),
    # These three raised `decimal.Inexact` / `decimal.Overflow` out of the
    # evaluator before.
    ("$degreesToRadians", "1E-6176", "0E-6176"),
    ("$degreesToRadians", "-1E-6176", "-0E-6176"),
    ("$radiansToDegrees", "1E-6176", "5.7E-6175"),
    ("$radiansToDegrees", "-1E-6176", "-5.7E-6175"),
    ("$radiansToDegrees", "1E+6144", "Infinity"),
    ("$radiansToDegrees", "-1E+6144", "-Infinity"),
    # 34 digits, not 32: the decimal path used to compute `x * pi / 180`, two
    # roundings where mongod does one.
    ("$degreesToRadians", "4.9E-324", "8.552113334772214926926084765594204E-326"),
    # The zero quantum tracks the ARGUMENT's, so a fixed table cannot serve it.
    ("$degreesToRadians", "0", "0E-35"),
    ("$degreesToRadians", "-0", "-0E-35"),
    ("$degreesToRadians", "-0.00", "-0E-37"),
    ("$radiansToDegrees", "0", "0E-32"),
    ("$radiansToDegrees", "-0.00", "-0E-34"),
]


@pytest.mark.parametrize("op,value,expected", DEG_RAD, ids=[f"{o}({v})" for o, v, _ in DEG_RAD])
def test_degrees_radians_matches_mongod(op, value, expected):
    assert str(apply(op, value)) == expected


# ---------------------------------------------------------------------------
# $exp: the tiny region is a bare 1, not the 34-digit form
# ---------------------------------------------------------------------------

EXP = [
    ("1E-6176", "1"),
    ("-1E-6176", "1"),
    ("1E-400", "1"),
    ("4.9E-324", "1"),
    ("1E+6144", "Infinity"),
    ("-1E+6144", "0E-6176"),
    ("0", "1"),
    ("1", "2.718281828459045235360287471352662"),
]


@pytest.mark.parametrize("value,expected", EXP, ids=[v for v, _ in EXP])
def test_exp_matches_mongod(value, expected):
    assert str(apply("$exp", value)) == expected

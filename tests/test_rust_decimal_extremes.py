"""Decimal128 values outside `f64`'s range, on the RUST server.

decimal128 spans `1E-6176` to `9.999…E+6144`; `f64` spans about `1E-308` to
`1E+308`. Six operators asked their classifying questions -- "is this
infinite?", "is this zero?" -- of an `f64` rendering of the argument, and that
rendering SATURATES: a finite `Decimal128("1E+6144")` reads back as
`f64::INFINITY` and a finite `Decimal128("1E-6176")` as `0.0`. Every one of them
took the wrong branch and answered confidently.

Measured against mongod 8.2.11 on 2026-09-07 over 18 operators x 10 extreme
inputs: **79 of 180 cells diverged**, and the eight operators pinned here are
now 0. Every expectation below is that server's own answer, copied verbatim.

The four that remain (`$ln`, `$log10`, `$sin`, `$atan` on extreme inputs) need
correctly-rounded decimal transcendentals and are tracked in
`tasks/backlog.md`; they are deliberately absent here rather than pinned wrong.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")
from bson import Decimal128  # noqa: E402


@pytest.fixture(scope="module")
def db(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_decext") / "wt"), 0)
    host, port = srv.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["decext"]
    d.c.insert_one({"_id": 1})
    try:
        yield d
    finally:
        cli.close()
        srv.stop()


def _apply(db, op, value):
    spec = {op: {"$literal": Decimal128(value)}}
    return list(db.c.aggregate([{"$addFields": {"x": spec}}]))[0]["x"]


# ---------------------------------------------------------------------------
# $sqrt -- correctly rounded, which IEEE 754 requires of square root
# ---------------------------------------------------------------------------

SQRT = [
    # Exact roots keep the ideal exponent floor(e/2), so these do not grow to
    # 34 digits.
    ("4", "2"),
    ("100", "10"),
    ("0.25", "0.5"),
    ("6.25", "2.5"),
    ("81", "9"),
    ("0", "0"),
    ("-0", "-0"),
    ("0.00", "0.0"),
    # Inexact roots are 34 significant digits.
    ("2", "1.414213562373095048801688724209698"),
    ("2.5", "1.581138830084189665999446772216359"),
    ("1E-10", "0.00001"),
    # The extremes: an `f64` route answered `Infinity` for all three.
    ("1E+6144", "1.00000000000000000E+3072"),
    ("1E+6111", "3.162277660168379331998893544432719E+3055"),
    (
        "9.999999999999999999999999999999999E+6144",
        "3.162277660168379331998893544432718E+3072",
    ),
    ("1E-6176", "1E-3088"),
    # Specials keep the decimal type.
    ("Infinity", "Infinity"),
    ("NaN", "NaN"),
]


@pytest.mark.parametrize("value,expected", SQRT, ids=[v for v, _ in SQRT])
def test_sqrt_matches_mongod(db, value, expected):
    assert str(_apply(db, "$sqrt", value)) == expected


@pytest.mark.parametrize("value", ["-1", "-0.5", "-1E+6144", "-Infinity"])
def test_sqrt_negative_is_a_domain_error(db, value):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _apply(db, "$sqrt", value)
    assert e.value.code == 28714
    assert "$sqrt's argument must be greater than or equal to 0" in str(e.value)


# ---------------------------------------------------------------------------
# $degreesToRadians / $radiansToDegrees -- one correctly-rounded multiply by
# mongod's own 34-digit constant, including into the subnormal range
# ---------------------------------------------------------------------------

DEG_RAD = [
    ("$degreesToRadians", "1", "0.01745329251994329576923690768488613"),
    ("$degreesToRadians", "180", "3.141592653589793238462643383279503"),
    ("$degreesToRadians", "0", "0E-35"),
    ("$degreesToRadians", "-0", "-0E-35"),
    ("$degreesToRadians", "1E+6144", "1.745329251994329576923690768488613E+6142"),
    ("$degreesToRadians", "1E+400", "1.745329251994329576923690768488613E+398"),
    # Underflows all the way to the minimum quantum.
    ("$degreesToRadians", "1E-6176", "0E-6176"),
    ("$degreesToRadians", "-1E-6176", "-0E-6176"),
    ("$degreesToRadians", "1E-6100", "1.745329251994329576923690768488613E-6102"),
    ("$radiansToDegrees", "1", "57.29577951308232087679815481410517"),
    ("$radiansToDegrees", "0", "0E-32"),
    ("$radiansToDegrees", "1E+6111", "5.729577951308232087679815481410517E+6112"),
    # Overflows to Infinity, and lands SUBNORMAL at the other end -- two digits
    # of the 34 survive.
    ("$radiansToDegrees", "1E+6144", "Infinity"),
    ("$radiansToDegrees", "-1E+6144", "-Infinity"),
    ("$radiansToDegrees", "1E-6176", "5.7E-6175"),
    ("$radiansToDegrees", "-1E-6176", "-5.7E-6175"),
]


@pytest.mark.parametrize("op,value,expected", DEG_RAD, ids=[f"{o}({v})" for o, v, _ in DEG_RAD])
def test_degrees_radians_matches_mongod(db, op, value, expected):
    assert str(_apply(db, op, value)) == expected


# ---------------------------------------------------------------------------
# $floor / $ceil -- the decimal spec's `quantize`, so past 34 integer digits
# the answer is NaN. $trunc / $round deliberately do NOT share that.
# ---------------------------------------------------------------------------

FLOOR_CEIL = [
    ("9999999999999999999999999999999999", "9999999999999999999999999999999999"),
    ("1E+33", "1000000000000000000000000000000000"),
    ("1E+19", "10000000000000000000"),
    ("1E+34", "NaN"),
    ("1E+6144", "NaN"),
    ("-1E+6144", "NaN"),
    ("1E-6176", None),  # floor 0, ceil 1 -- checked separately
]


@pytest.mark.parametrize(
    "value,expected", [(v, e) for v, e in FLOOR_CEIL if e is not None], ids=lambda v: str(v)
)
def test_floor_ceil_quantize_to_nan_past_34_digits(db, value, expected):
    assert str(_apply(db, "$floor", value)) == expected
    assert str(_apply(db, "$ceil", value)) == expected


@pytest.mark.parametrize("value", ["1E+34", "1E+6144"])
def test_trunc_and_round_do_not_quantize(db, value):
    """The asymmetry that keeps the rule out of `round_to_exp`."""
    assert str(_apply(db, "$trunc", value)) != "NaN"
    assert str(_apply(db, "$round", value)) != "NaN"


# ---------------------------------------------------------------------------
# $toBool -- a decimal below f64's range is nonzero, so it is true
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("value", ["1E-6176", "-1E-6176", "1E-6100", "1E-400"])
def test_tiny_decimal_is_truthy(db, value):
    assert _apply(db, "$toBool", value) is True


@pytest.mark.parametrize("value", ["0", "-0", "0E-6176", "0.000"])
def test_decimal_zero_is_falsy(db, value):
    assert _apply(db, "$toBool", value) is False


# ---------------------------------------------------------------------------
# $toDouble -- converts only when the rounded double is NORMAL. mongod raises
# 241 on overflow AND on a subnormal, so a representable subnormal still fails.
# ---------------------------------------------------------------------------

TO_DOUBLE_OK = [
    ("0", 0.0),
    ("-0", -0.0),
    ("0E-6176", 0.0),
    ("1", 1.0),
    ("2.2250738585072014E-308", 2.2250738585072014e-308),
    ("2.2250738585072013E-308", 2.2250738585072014e-308),
    ("1.7976931348623158E+308", 1.7976931348623157e308),
    # Just ABOVE the tininess cut, which sits a quarter of a subnormal ULP
    # below f64::MIN_POSITIVE. Bisected against 8.2.11 on 2026-09-07.
    ("2.2250738585072012595738212570267910E-308", 2.2250738585072014e-308),
]


@pytest.mark.parametrize("value,expected", TO_DOUBLE_OK, ids=[v for v, _ in TO_DOUBLE_OK])
def test_to_double_converts_normals_and_zero(db, value, expected):
    got = _apply(db, "$toDouble", value)
    assert got == expected
    assert str(got) == str(expected)  # keeps the sign of a zero


TO_DOUBLE_241 = [
    "1E+6144",
    "1E+310",
    "1E+400",
    "-1E+6144",
    "1.797693134862315808E+308",
    "1E-6176",
    "1E-400",
    "2.2250738585072012E-308",
    "4.9E-324",  # representable as a SUBNORMAL double, and still a 241
    # Just BELOW the cut. Both of these parse to f64::MIN_POSITIVE, so
    # `is_normal()` on the parsed double cannot tell them from the row above.
    "2.2250738585072012595738212569813160E-308",
    "2.22507385850720125E-308",
    "2.225073858507201259573821257020768E-308",  # the cut TRUNCATED: still below
]


@pytest.mark.parametrize("value", TO_DOUBLE_241)
def test_to_double_refuses_overflow_and_subnormal(db, value):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        _apply(db, "$toDouble", value)
    assert e.value.code == 241
    assert "Conversion would overflow target type" in str(e.value)


@pytest.mark.parametrize(
    "value,expected", [("Infinity", "inf"), ("-Infinity", "-inf"), ("NaN", "nan")]
)
def test_to_double_passes_specials_through(db, value, expected):
    assert str(_apply(db, "$toDouble", value)) == expected


# ---------------------------------------------------------------------------
# $exp -- the two regions where the answer needs no series at all
# ---------------------------------------------------------------------------

EXP = [
    ("1E+5", "Infinity"),
    ("1E+400", "Infinity"),
    ("1E+6144", "Infinity"),
    ("9.999999999999999999999999999999999E+6144", "Infinity"),
    # Underflow lands on the MINIMUM quantum, not a bare zero.
    ("-1E+5", "0E-6176"),
    ("-1E+6144", "0E-6176"),
    # Forty orders below the resolution at 1.
    ("1E-6176", "1"),
    ("-1E-6176", "1"),
    ("1E-6100", "1"),
    ("1E-400", "1"),
    # Zero and the specials were already right.
    ("0", "1"),
    ("-0", "1"),
    ("Infinity", "Infinity"),
    ("-Infinity", "0"),
    ("NaN", "NaN"),
]


@pytest.mark.parametrize("value,expected", EXP, ids=[v for v, _ in EXP])
def test_exp_extremes_match_mongod(db, value, expected):
    assert str(_apply(db, "$exp", value)) == expected

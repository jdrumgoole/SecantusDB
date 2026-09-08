"""Conversions, decimal `$divide` / `$mod`, and missing-parameter errors.

Found by re-running `tools/probes/agg_expressions.py` (6,628 cases) against the
Rust server: 28 shapes answered a different code from mongod and several of
those were a wrong VALUE, not a wrong message. Extending the same cases to the
pure Python engine found the same family plus three of its own, including a raw
`decimal.DivisionByZero` escaping the evaluator.

Every expectation is measured from mongod 8.2.11 on 2026-09-08. The grid of
mongod × Rust server × Python engine over these 76 cases is 0 divergent.
"""

from __future__ import annotations

import datetime

import pytest
from bson import Binary, Decimal128

from secantus.expressions import ExpressionError, evaluate

WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)


def ev(spec):
    return evaluate(spec, {})


def D(v):
    return {"$literal": Decimal128(v)}


def B(raw, subtype=0):
    return {"$literal": Binary(raw, subtype)}


# ---------------------------------------------------------------------------
# Missing required parameters. These answered NULL, not an error.
# ---------------------------------------------------------------------------

MISSING = [
    ({"$dateFromString": {}}, 40542, "Missing 'dateString' parameter to $dateFromString"),
    ({"$dateToParts": {}}, 40522, "Missing 'date' parameter to $dateToParts"),
    ({"$dateTrunc": {}}, 5439009, "Missing 'date' parameter to $dateTrunc"),
    ({"$dateTrunc": {"date": WHEN}}, 5439010, "Missing 'unit' parameter to $dateTrunc"),
    (
        {"$dateDiff": {"startDate": WHEN, "unit": "day"}},
        5166304,
        "Missing 'endDate' parameter to $dateDiff",
    ),
    (
        {"$dateDiff": {"startDate": WHEN, "endDate": WHEN}},
        5166305,
        "Missing 'unit' parameter to $dateDiff",
    ),
    # `field` outranks `input`, the reverse of the order they are read in.
    ({"$getField": {}}, 3041702, "$getField requires 'field' to be specified"),
    (
        {"$getField": {"input": {"a": 1}}},
        3041702,
        "$getField requires 'field' to be specified",
    ),
]


@pytest.mark.parametrize("spec,code,message", MISSING, ids=[str(s)[:38] for s, _, _ in MISSING])
def test_missing_parameter_raises_rather_than_answering_null(spec, code, message):
    with pytest.raises(ExpressionError) as e:
        ev(spec)
    assert (e.value.code, str(e.value)) == (code, message)


# ---------------------------------------------------------------------------
# $divide: the decimal spec's IDEAL EXPONENT is what separates these
# ---------------------------------------------------------------------------

DIVIDE = [
    ("2.5", "1", "2.5"),
    ("10", "4", "2.5"),
    ("1", "8", "0.125"),
    ("100", "10", "10"),
    ("2.50", "1.0", "2.5"),
    ("7", "2", "3.5"),
    ("0", "5", "0"),
    ("-0", "5", "-0"),
    ("1", "3", "0.3333333333333333333333333333333333"),
    ("1", "7", "0.1428571428571428571428571428571429"),
    ("2", "3", "0.6666666666666666666666666666666667"),
    # Range: the format's own overflow and minimum quantum.
    ("1E+6144", "1E-10", "Infinity"),
    ("1E-6176", "10", "0E-6176"),
]


@pytest.mark.parametrize("a,b,expected", DIVIDE, ids=[f"{a}/{b}" for a, b, _ in DIVIDE])
def test_divide_matches_mongod(a, b, expected):
    assert str(ev({"$divide": [D(a), D(b)]})) == expected


def test_divide_promotes_a_mixed_pair_to_decimal():
    assert str(ev({"$divide": [D("2.5"), 2]})) == "1.25"


def test_divide_by_a_decimal_zero_is_an_error_not_a_crash():
    """`b == 0` is FALSE for `Decimal128("0")`, so this used to raise
    `decimal.DivisionByZero` out of the evaluator."""
    with pytest.raises(ExpressionError) as e:
        ev({"$divide": [D("1"), D("0")]})
    assert (e.value.code, str(e.value)) == (2, "can't $divide by zero")


# ---------------------------------------------------------------------------
# $mod: sign follows the dividend, quantum is min(e1, e2)
# ---------------------------------------------------------------------------

MOD = [
    ("2.5", "1", "0.5"),
    ("10", "3", "1"),
    ("-10", "3", "-1"),
    ("10", "-3", "1"),
    ("7.5", "2.5", "0.0"),
    # Exact over 6,145 digits. This answered `NaN` when the quotient exceeded
    # the 34-digit working precision.
    ("1E+6144", "7", "1"),
]


@pytest.mark.parametrize("a,b,expected", MOD, ids=[f"{a}%{b}" for a, b, _ in MOD])
def test_mod_matches_mongod(a, b, expected):
    assert str(ev({"$mod": [D(a), D(b)]})) == expected


def test_mod_by_zero_code_depends_on_whether_a_decimal_is_involved():
    """16610 for int / double operands, 5733415 once a decimal is on either
    side -- whatever the other one is."""
    with pytest.raises(ExpressionError) as e:
        ev({"$mod": [D("2.5"), 0]})
    assert e.value.code == 5733415
    with pytest.raises(ExpressionError) as e:
        ev({"$mod": [1, 0]})
    assert e.value.code == 16610


# ---------------------------------------------------------------------------
# binData conversions: reinterpreted, never parsed
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "raw,expected",
    [
        (b"z", 122),
        (b"zz", 31354),
        (b"zzzz", 2054847098),
        # LITTLE-endian, measured: 0x04030201, not 0x01020304.
        (b"\x01\x02\x03\x04", 67305985),
    ],
)
def test_bindata_to_int_is_little_endian(raw, expected):
    assert ev({"$toInt": B(raw)}) == expected
    assert ev({"$toLong": B(raw)}) == expected


def test_bindata_to_long_takes_eight_bytes_and_to_int_does_not():
    assert ev({"$toLong": B(b"zzzzzzzz")}) == 8825501086245354106
    with pytest.raises(ExpressionError) as e:
        ev({"$toInt": B(b"zzzzzzzz")})
    assert e.value.code == 241
    assert "invalid length: 8" in str(e.value)


@pytest.mark.parametrize("raw", [b"zzz", b"", b"zzzzz"])
def test_bindata_of_an_unusable_length_names_the_length(raw):
    with pytest.raises(ExpressionError) as e:
        ev({"$toInt": B(raw)})
    assert e.value.code == 241
    assert f"invalid length: {len(raw)}" in str(e.value)
    hexed = "".join(f"{b:02X}" for b in raw)
    assert f"'BinData(0, \"{hexed}\")'" in str(e.value)


def test_bindata_to_double_reinterprets_the_bytes():
    """4 bytes are an IEEE single widened, 8 a double -- and 1 / 2, which the
    integer targets accept, are refused here."""
    assert ev({"$toDouble": B(b"zzzz")}) == pytest.approx(3.251395836102948e35)
    assert ev({"$toDouble": B(b"zzzzzzzz")}) == pytest.approx(9.61276249046606e281)
    for raw in (b"z", b"zz"):
        with pytest.raises(ExpressionError) as e:
            ev({"$toDouble": B(raw)})
        assert e.value.code == 241


@pytest.mark.parametrize(
    "raw,expected", [(b"z", "eg=="), (b"zz", "eno="), (b"zzz", "enp6"), (b"", "")]
)
def test_bindata_to_string_is_base64(raw, expected):
    assert ev({"$toString": B(raw)}) == expected


def test_bindata_subtype_is_ignored():
    assert ev({"$toInt": B(b"z", 4)}) == 122
    assert ev({"$toString": B(b"\x01\x02\x03\x04", 4)}) == "AQIDBA=="


# ---------------------------------------------------------------------------
# $toDecimal of the non-finite doubles; $toDate of a decimal
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "value,expected",
    [(float("inf"), "Infinity"), (float("-inf"), "-Infinity"), (float("nan"), "NaN")],
)
def test_to_decimal_converts_the_non_finite_doubles(value, expected):
    assert str(ev({"$toDecimal": {"$literal": value}})) == expected


@pytest.mark.parametrize(
    "value,code,fragment",
    [
        ("Infinity", 241, "Attempt to convert infinity value to integer type"),
        ("-Infinity", 241, "Attempt to convert infinity value to integer type"),
        ("NaN", 241, "Attempt to convert NaN value to integer type"),
        # The decimal renders in ITS form (`1E+30`), not the double formatter's.
        ("1E+30", 241, "with no onError value: 1E+30"),
    ],
)
def test_to_date_of_an_unusable_decimal(value, code, fragment):
    with pytest.raises(ExpressionError) as e:
        ev({"$toDate": D(value)})
    assert e.value.code == code
    assert fragment in str(e.value)


@pytest.mark.parametrize("value", [1.5, Decimal128("1.5")])
def test_to_date_truncates_toward_zero(value):
    """A BSON date holds WHOLE milliseconds. Passing the fraction through built
    a datetime with 1500 microseconds -- a value BSON cannot hold."""
    got = ev({"$toDate": {"$literal": value}})
    assert got == datetime.datetime(1970, 1, 1, 0, 0, 0, 1000)


@pytest.mark.parametrize("value", [-1.5, Decimal128("-1.5")])
def test_to_date_truncates_negatives_toward_zero(value):
    got = ev({"$toDate": {"$literal": value}})
    assert got == datetime.datetime(1969, 12, 31, 23, 59, 59, 999000)

"""``$convert`` and the ``$toX`` shorthands, pinned against mongod 8.2.11.

Measured on 2026-09-01 with ``tools/probes/`` style sweeps: 480 shapes across
``$convert`` and the eight shorthands, plus 51 date / objectId shapes, all
agreeing at the time this file was written.

Three of the fixes behind it are WRONG VALUES rather than wrong messages, which
is why this file exists at all:

* ``$toInt: " 5 "`` returned ``5``. Python's ``int()`` strips whitespace and
  accepts PEP-515 underscores; mongod's parser accepts neither.
* ``$toDate: 1`` returned an epoch date. mongod converts a LONG and refuses an
  int32 outright.
* ``$convert("" -> bool)`` returned ``False``. Every string is true in BSON,
  the empty one included.

and one is a CRASH: ``Decimal128("Infinity")`` to an integer target reached
``int(Decimal("Infinity"))``, whose ``OverflowError`` escaped as
``1 internal server error``.
"""

from __future__ import annotations

import datetime as dt

import pytest
from bson import Decimal128, Int64, ObjectId, Timestamp

from secantus.expressions import ExpressionError, evaluate


def _err(expr):
    with pytest.raises(ExpressionError) as exc:
        evaluate(expr, {})
    return exc.value


@pytest.mark.parametrize(
    ("text", "reason"),
    [
        ("", "No digits"),
        ("x", "Did not consume whole string."),
        ("12abc", "Did not consume whole string."),
        ("1.5", "Did not consume whole string."),
        (" 5 ", "Did not consume whole string."),
        ("5 ", "Did not consume whole string."),
        (" 5", "Did not consume whole string."),
        ("1e3", "Did not consume whole string."),
        ("1_0", "Did not consume whole string."),
        (".5", "Did not consume whole string."),
        ("5.", "Did not consume whole string."),
        ("１２", "Did not consume whole string."),
        ("999999999999999999999999", "Overflow"),
        ("2147483648", "Overflow"),
    ],
)
def test_to_int_string_rejections(text, reason):
    err = _err({"$toInt": text})
    assert err.code == 241
    assert err.code_name == "ConversionFailure"
    assert str(err) == (
        f"Failed to parse number '{text}' in $convert with no onError value: {reason}"
    )


@pytest.mark.parametrize("text", ["+5", "-5", "0", "-0", "007"])
def test_to_int_accepts_what_mongod_accepts(text):
    assert evaluate({"$toInt": text}, {}) == int(text)


def test_hexadecimal_has_its_own_message_shape():
    """The input is named AFTER the clause and unquoted, unlike every other
    conversion failure."""
    err = _err({"$toInt": "0x10"})
    assert err.code == 241
    assert str(err) == "Illegal hexadecimal input in $convert with no onError value: 0x10"


@pytest.mark.parametrize(
    ("text", "reason"),
    [
        ("", "Empty string"),
        ("x", "Did not consume any digits"),
        ("１２", "Did not consume any digits"),
        ("12abc", "Did not consume whole string."),
        (" 5", "Leading whitespace"),
        ("  ", "Leading whitespace"),
        ("5 ", "Did not consume whole string."),
    ],
)
def test_to_double_string_rejections(text, reason):
    """double has FOUR reasons where int has three, and they do not line up."""
    err = _err({"$toDouble": text})
    assert str(err).endswith(f": {reason}")


@pytest.mark.parametrize("text", ["1e3", "inf", "Infinity", ".5", "5.", "+5", "1.5"])
def test_to_double_accepts_the_strtod_forms(text):
    assert isinstance(evaluate({"$toDouble": text}, {}), float)


def test_to_decimal_has_a_single_reason():
    assert str(_err({"$toDecimal": "x"})).endswith(": Failed to parse string to decimal")
    assert str(_err({"$toDecimal": ""})).endswith(": Empty string")


@pytest.mark.parametrize(
    ("value", "message"),
    [
        (float("nan"), "Attempt to convert NaN value to integer type in $convert"),
        (float("inf"), "Attempt to convert infinity value to integer type in $convert"),
        (Decimal128("NaN"), "Attempt to convert NaN value to integer type in $convert"),
        (
            Decimal128("Infinity"),
            "Attempt to convert infinity value to integer type in $convert",
        ),
    ],
)
def test_non_finite_to_integer_is_not_an_overflow(value, message):
    """mongod separates NaN, infinity and out-of-range; one overflow message
    used to cover all three -- and the Decimal128 infinity CRASHED."""
    err = _err({"$toInt": value})
    assert err.code == 241
    assert str(err) == f"{message} with no onError value"


def test_numeric_overflow_names_the_value():
    err = _err({"$toInt": 1e300})
    assert str(err) == (
        "Conversion would overflow target type in $convert with no onError value: 1e+300"
    )


def test_unsupported_source_type_is_conversion_failure():
    """Not a TypeMismatch: mongod answers 241 and names both ends."""
    err = _err({"$convert": {"input": [1, 2], "to": "int"}})
    assert err.code == 241
    assert str(err) == "Unsupported conversion from array to int in $convert with no onError value"


def test_to_string_uses_bson_spellings_not_python_ones():
    assert evaluate({"$toString": True}, {}) == "true"
    assert evaluate({"$toString": False}, {}) == "false"
    assert evaluate({"$toString": float("inf")}, {}) == "Infinity"
    assert evaluate({"$toString": float("-inf")}, {}) == "-Infinity"
    assert evaluate({"$toString": float("nan")}, {}) == "NaN"


def test_every_string_is_true():
    assert evaluate({"$toBool": ""}, {}) is True
    assert evaluate({"$toBool": "x"}, {}) is True


def test_to_date_rejects_an_int32_and_accepts_a_long():
    err = _err({"$toDate": 1})
    assert err.code == 241
    assert str(err) == "Unsupported conversion from int to date in $convert with no onError value"
    assert evaluate({"$toDate": Int64(1577836800000)}, {}) == dt.datetime(2020, 1, 1)


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("2020-01-01", dt.datetime(2020, 1, 1)),
        ("2020-01-01T00:00:00Z", dt.datetime(2020, 1, 1)),
        ("2020-01-01T00:00:00.123Z", dt.datetime(2020, 1, 1, 0, 0, 0, 123000)),
        # A sub-millisecond fraction is TRUNCATED, not rejected.
        ("2020-01-01T00:00:00.1234567Z", dt.datetime(2020, 1, 1, 0, 0, 0, 123000)),
        ("2020-01-01 00:00:00", dt.datetime(2020, 1, 1)),
        ("2020-01-01T00:00:00+02:00", dt.datetime(2019, 12, 31, 22, 0)),
        (" 2020-01-01", dt.datetime(2020, 1, 1)),
        ("2020-01-01 ", dt.datetime(2020, 1, 1)),
        ("2020-01", dt.datetime(2020, 1, 1)),
        (Timestamp(1, 1), dt.datetime(1970, 1, 1, 0, 0, 1)),
        (ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), dt.datetime(2023, 7, 19, 14, 56, 34)),
    ],
)
def test_to_date_values(value, expected):
    assert evaluate({"$toDate": value}, {}) == expected


def test_to_date_result_is_naive_so_it_compares_with_a_stored_date():
    """A tz-AWARE result raises ``TypeError`` against a date read out of a
    document, which is the common comparison."""
    assert evaluate({"$toDate": Int64(0)}, {}).tzinfo is None


def test_to_object_id_exists_and_reports_mongods_length_error():
    oid = "64b7f9a2c1d2e3f4a5b6c7d8"
    assert evaluate({"$toObjectId": oid}, {}) == ObjectId(oid)
    err = _err({"$toObjectId": "x"})
    assert err.code == 241
    assert str(err) == (
        "Failed to parse objectId 'x' in $convert with no onError value: "
        "Invalid string length for parsing to OID, expected 24 but found 1"
    )


@pytest.mark.parametrize(
    "op", ["$toInt", "$toLong", "$toDouble", "$toDecimal", "$toString", "$toBool"]
)
def test_conversion_shorthands_take_the_single_element_array_form(op):
    """``{$toInt: ["$s"]}`` is how every field reference is naturally written;
    it used to try to convert the ARRAY."""
    assert evaluate({op: ["7"]}, {}) == evaluate({op: "7"}, {})


@pytest.mark.parametrize("arg", [[], [1, 2], [1, 2, 3]])
def test_conversion_shorthands_have_their_own_arity_error(arg):
    err = _err({"$toInt": arg})
    assert err.code == 50723
    assert str(err) == f"$toInt requires a single argument, got {len(arg)}"


def test_on_error_still_catches_a_parse_failure():
    assert evaluate({"$convert": {"input": "x", "to": "int", "onError": "nope"}}, {}) == "nope"


def test_std_dev_in_expression_position():
    """The accumulator forms shipped; the EXPRESSION forms answered
    ``Unknown expression`` where mongod computes (probed 8.2.11)."""
    assert evaluate({"$stdDevPop": [1, 2, 3]}, {}) == pytest.approx(0.816496580927726)
    assert evaluate({"$stdDevSamp": [1, 2, 3]}, {}) == 1.0
    # Same emptiness rules as the accumulators.
    assert evaluate({"$stdDevPop": []}, {}) is None
    assert evaluate({"$stdDevSamp": [5]}, {}) is None
    assert evaluate({"$stdDevPop": [5]}, {}) == 0.0

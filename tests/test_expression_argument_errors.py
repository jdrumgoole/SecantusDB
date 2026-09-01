"""mongod's error surface for a bad ARGUMENT to a perfectly ordinary operator.

Every code and message here was measured against mongod 8.2.11 on 2026-09-01
(``tools/probes/`` style sweeps run from a scratch script), and each one was a
real divergence before this file existed. The ones marked as wrong *answers*
matter most: they returned a value where the server refuses, so a caller saw
plausible data instead of an error.

The Rust server has to name these itself -- it has no Python engine behind it,
so an argument error that only the pure engine could state reached a client as
"not supported by the Rust server". ``tests/test_rust_expressions_parity.py``
pins the two engines to each other; this file pins the Python one to mongod.
"""

from __future__ import annotations

import datetime

import pytest
from bson import Decimal128, Int64
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def coll(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            yield client["testdb"]["things"]
        finally:
            client.close()


def _value(coll, expr, doc=None):
    coll.delete_many({})
    coll.insert_one(doc or {"_id": 1})
    return list(coll.aggregate([{"$project": {"v": expr}}]))[0].get("v", "<absent>")


def _error(coll, expr, doc=None):
    with pytest.raises(OperationFailure) as exc:
        _value(coll, expr, doc)
    detail = exc.value.details or {}
    return detail.get("code"), detail.get("errmsg", "").split(":: caused by :: ")[-1]


# --- $round / $trunc precision -------------------------------------------
# mongod validates a precision in three steps, each with its own code:
# coerceToLong (16004 / 31109), Value::integral() (51082), then [-20, 100]
# (51083). Only the fractional-double half of step 2 was implemented, so a
# fractional Decimal128, an out-of-int32 integer, a string and a bool were all
# silently ACCEPTED -- {$round: ["$n", -25]} answered 0.0.
@pytest.mark.parametrize(
    "precision,code,fragment",
    [
        (1.5, 51082, "precision argument to  $round must be a integral value"),
        (Decimal128("1.5"), 51082, "must be a integral value"),
        (10_000_000_000, 51082, "must be a integral value"),
        (2**31, 51082, "must be a integral value"),
        (2**31 - 1, 51083, "value must be in [-20, 100]"),
        (Int64(200), 51083, "cannot apply $round with precision value 200"),
        (-25, 51083, "cannot apply $round with precision value -25"),
        (101, 51083, "value must be in [-20, 100]"),
        ("x", 16004, "can't convert from BSON type string to long"),
        (True, 16004, "can't convert from BSON type bool to long"),
        (float("nan"), 31109, "Can't coerce out of range value nan to long"),
        (float("inf"), 31109, "Can't coerce out of range value inf to long"),
    ],
)
def test_round_precision_is_validated(coll, precision, code, fragment):
    got_code, message = _error(coll, {"$round": [7.5, precision]})
    assert got_code == code
    assert fragment in message


@pytest.mark.parametrize("precision", [0, 2, 100, -20, 2.0, Decimal128("2"), None])
def test_round_precision_accepted(coll, precision):
    # A null precision makes the whole operator null, which is NOT a precision
    # of 0 -- the old code conflated them.
    result = _value(coll, {"$round": [7.5, precision]})
    assert result is None if precision is None else isinstance(result, float)


def test_trunc_shares_the_precision_rules(coll):
    code, message = _error(coll, {"$trunc": [7.5, -25]})
    assert code == 51083
    assert "cannot apply $trunc with precision value -25" in message


# --- $indexOfArray -------------------------------------------------------
# Its own codes (9711600 / 9711601), not the string forms' 40096 / 40097. A
# negative start used to be CLAMPED, so this answered 2.
def test_index_of_array_rejects_a_negative_index(coll):
    code, message = _error(coll, {"$indexOfArray": [[1, 2, 3], 3, -1]})
    assert code == 9711601
    assert message == "$indexOfArray requires a nonnegative starting index, found: -1"


def test_index_of_array_rejects_a_non_numeric_index(coll):
    # This silently answered -1.
    code, message = _error(coll, {"$indexOfArray": [[1, 2, 3], 3, "x"]})
    assert code == 9711600
    assert 'with value: "x"' in message


def test_index_of_string_forms_keep_the_older_codes(coll):
    assert _error(coll, {"$indexOfCP": ["abc", "c", -1]})[0] == 40097
    assert _error(coll, {"$indexOfBytes": ["abc", "c", 1.5]})[0] == 40096


# --- $range --------------------------------------------------------------
def test_range_zero_step(coll):
    code, message = _error(coll, {"$range": [0, 5, 0]})
    assert code == 34449
    assert message == "$range requires a non-zero step value"


# --- $dateToString -------------------------------------------------------
# The format language is NOT strftime. Handing it to strftime accepted
# directives mongod refuses (so a typo produced a wrong string, not an error)
# and left %z / %Z empty.
@pytest.mark.parametrize(
    "fmt,expected",
    [
        ("%G-W%V-%u", "2020-W53-7"),
        ("%U", "01"),
        ("%z", "+0000"),
        ("%Z", "0"),
        ("%b %B", "Jan January"),
    ],
)
def test_date_to_string_directives(coll, fmt, expected):
    doc = {"_id": 1, "d": datetime.datetime(2021, 1, 3)}
    assert _value(coll, {"$dateToString": {"date": "$d", "format": fmt}}, doc) == expected


def test_date_to_string_sunday_week_leap_year_edge(coll):
    # 2012-12-31 is a Monday in a leap year that began on a Sunday: week 53.
    doc = {"_id": 1, "d": datetime.datetime(2012, 12, 31)}
    assert _value(coll, {"$dateToString": {"date": "$d", "format": "%U"}}, doc) == "53"


def test_date_to_string_rejects_an_unknown_directive(coll):
    doc = {"_id": 1, "d": datetime.datetime(2021, 1, 3)}
    with pytest.raises(OperationFailure) as exc:
        _value(coll, {"$dateToString": {"date": "$d", "format": "%a"}}, doc)
    assert exc.value.code == 18536
    assert "Invalid format character '%a' in format string" in str(exc.value)


def test_date_to_string_offset_directives_follow_the_timezone(coll):
    doc = {"_id": 1, "d": datetime.datetime(2021, 1, 3, 14)}
    spec = {"date": "$d", "format": "%z|%Z|%H", "timezone": "+05:30"}
    assert _value(coll, {"$dateToString": spec}, doc) == "+0530|330|19"


# --- ASCII case and trim -------------------------------------------------
# mongod's case operators map ASCII ONLY; Python's .upper() folds ß to SS and
# strips accents' case, and .strip() removes whitespace mongod leaves.
@pytest.mark.parametrize(
    "expr,expected",
    [
        ({"$toUpper": "Ünïcodé"}, "ÜNïCODé"),
        ({"$toUpper": "straße"}, "STRAßE"),
        ({"$toLower": "ΣΊΣΥΦΟΣ"}, "ΣΊΣΥΦΟΣ"),
        ({"$strcasecmp": ["ß", "SS"]}, 1),
        ({"$trim": {"input": "　pad　"}}, "　pad　"),
        ({"$trim": {"input": " pad "}}, " pad "),
        ({"$trim": {"input": "  pad\t\n"}}, "pad"),
        ({"$ltrim": {"input": " pad "}}, "pad "),
    ],
)
def test_ascii_case_and_fixed_whitespace_trim(coll, expr, expected):
    assert _value(coll, expr) == expected


# --- numeric-string conversion -------------------------------------------
# mongod's parser is C's strtod, which is stricter than Python's float() about
# whitespace and underscores and LOOSER about hexadecimal.
@pytest.mark.parametrize(
    "text,expected",
    [
        ("0X1f", 31.0),  # escapes the "0x" gate, then strtod reads hex
        ("-0x10", -16.0),
        ("+0x10", 16.0),
        ("1e3", 1000.0),
        ("inf", float("inf")),
    ],
)
def test_to_double_accepts_what_strtod_accepts(coll, text, expected):
    assert _value(coll, {"$toDouble": text}) == expected


@pytest.mark.parametrize(
    "text,fragment",
    [
        ("0x10", "Illegal hexadecimal input"),  # the gate is a literal "0x"
        (" 5 ", "Leading whitespace"),
        ("1_000", "Did not consume whole string."),
        ("abc", "Did not consume any digits"),
        ("1e400", "Out of range"),  # answered inf before
        ("", "Empty string"),
    ],
)
def test_to_double_rejects_what_strtod_rejects(coll, text, fragment):
    code, message = _error(coll, {"$toDouble": text})
    assert code == 241
    assert fragment in message


@pytest.mark.parametrize(
    "text,fragment",
    [
        ("0X1f", "Did not consume whole string."),
        ("", "No digits"),
        ("2147483648", "Overflow"),
        (" 5 ", "Did not consume whole string."),
    ],
)
def test_to_int_string_reasons_differ_from_double(coll, text, fragment):
    code, message = _error(coll, {"$toInt": text})
    assert code == 241
    assert fragment in message


def test_overflow_message_renders_the_value_mongods_way(coll):
    # A double takes %g; an int64 names NOTHING.
    assert "1e+10" in _error(coll, {"$toInt": 1e10})[1]
    assert _error(coll, {"$toInt": Int64(2**31)})[1].endswith("no onError value: ")


@pytest.mark.parametrize(
    "text,fragment",
    [
        ("z" * 24, "Invalid character found in hex string: z"),
        ("507f1f77bcf86cd7994390", "expected 24 but found 22"),
    ],
)
def test_to_object_id_names_the_right_fault(coll, text, fragment):
    code, message = _error(coll, {"$toObjectId": text})
    assert code == 241
    assert fragment in message


def test_conversion_shorthands_agree_with_convert(coll):
    # They ARE $convert with that target; keeping separate implementations is
    # how $toBool and $convert{to:"bool"} came to disagree on the empty string.
    for target, shorthand in [
        ("int", "$toInt"),
        ("long", "$toLong"),
        ("double", "$toDouble"),
        ("bool", "$toBool"),
        ("string", "$toString"),
        ("objectId", "$toObjectId"),
    ]:
        for value in ["", "5", "abc", "507f1f77bcf86cd799439011"]:
            expr_short = {shorthand: value}
            expr_full = {"$convert": {"input": value, "to": target}}
            try:
                short = _value(coll, expr_short)
                short_err = None
            except OperationFailure as e:
                short, short_err = None, (e.code, str(e).split(", full error")[0])
            try:
                full = _value(coll, expr_full)
                full_err = None
            except OperationFailure as e:
                full, full_err = None, (e.code, str(e).split(", full error")[0])
            assert (short, short_err) == (full, full_err), f"{shorthand} vs {target} on {value!r}"


# --- math domain guards --------------------------------------------------
@pytest.mark.parametrize(
    "expr,code",
    [
        ({"$sqrt": -1}, 28714),
        ({"$ln": 0}, 28766),
        ({"$log10": -1}, 28761),
        ({"$log": [0, 2]}, 28758),
        ({"$log": [10, 1]}, 28759),
        ({"$pow": [0, -1]}, 28764),
    ],
)
def test_math_domain_errors(coll, expr, code):
    assert _error(coll, expr)[0] == code

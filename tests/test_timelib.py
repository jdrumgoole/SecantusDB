"""The pure-Python timelib port (``secantus.timelib``).

Mirrors ``crates/secantus-core/src/timelib/tests.rs``. Each expectation was
measured on mongod 8.2.11 (2026-10-06) with ``$toDate: <string>`` or
``$dateFromString`` -- the full error text, or the resulting instant. mongod is
the authority here, not the Rust port: these are the same measurements, run
against the second translation.
"""

from __future__ import annotations

import pytest

from secantus.timelib import (
    TimelibError,
    millis,
    mongo_parse,
    mongo_parse_format,
    update_ts,
    validate_format,
)


def parse_ms(s: str) -> int:
    t = mongo_parse(s)
    update_ts(t)
    ms = millis(t)
    assert ms is not None
    return ms


def format_ms(s: str, f: str) -> int:
    t = mongo_parse_format(s, f)
    update_ts(t)
    ms = millis(t)
    assert ms is not None
    return ms


def at(y: int, mo: int, d: int, h: int, mi: int, s: int) -> int:
    """Milliseconds since the epoch of a UTC wall-clock time (days_from_civil)."""
    y -= 1 if mo <= 2 else 0
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    doy = (153 * (mo + (-3 if mo > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    days = era * 146_097 + doe - 719_468
    return (days * 86_400 + h * 3600 + mi * 60 + s) * 1000


def err(s: str) -> str:
    with pytest.raises(TimelibError) as exc:
        parse_ms(s)
    return str(exc.value)


def format_err(s: str, f: str) -> str:
    with pytest.raises(TimelibError) as exc:
        format_ms(s, f)
    return str(exc.value)


@pytest.mark.parametrize(
    ("text", "message"),
    [
        (
            "abc",
            "Error parsing date string 'abc'; 0: passing a time zone identifier"
            " as part of the string is not allowed 'a'",
        ),
        ("", "Error parsing date string ''; 0: Empty string '\0'"),
        ("2024-13-01", "Error parsing date string '2024-13-01'; 6: Unexpected character '3'"),
        (
            "2024-02-30",
            "Error parsing date string '2024-02-30'; 11: The parsed date was invalid '\0'",
        ),
        (
            "2024-01-01T25:00",
            "Error parsing date string '2024-01-01T25:00'; 12: Double time specification '5'",
        ),
        (
            "hello world",
            "Error parsing date string 'hello world'; 0: passing a time zone identifier"
            " as part of the string is not allowed 'h'; 6: Double timezone specification 'w'",
        ),
        (
            "abc def ghi",
            "Error parsing date string 'abc def ghi'; 0: passing a time zone identifier"
            " as part of the string is not allowed 'a'; 8: Double timezone specification 'g';"
            " 4: Double timezone specification 'd'",
        ),
        (
            "2024-01-01T00:00:00+25:00",
            "Error parsing date string '2024-01-01T00:00:00+25:00'; 22: Unexpected character ':';"
            " 23: Unexpected character '0'; 24: Unexpected character '0'",
        ),
        (
            "@@@",
            "Error parsing date string '@@@'; 0: Unexpected character '@';"
            " 1: Unexpected character '@'; 2: Unexpected character '@'",
        ),
        ("x", 'an incomplete date/time string has been found, with elements missing: "x"'),
        (
            "12:00",
            'an incomplete date/time string has been found, with elements missing: "12:00"',
        ),
        (
            "2024-01-01 23:59:60",
            "Error parsing date string '2024-01-01 23:59:60'; 20: The parsed time was invalid '\0'",
        ),
    ],
)
def test_error_text_matches_mongod(text: str, message: str) -> None:
    assert err(text) == message


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("2024-01-01 10:00 PM", at(2024, 1, 1, 22, 0, 0)),
        ("2024-01-01 12:00 AM", at(2024, 1, 1, 0, 0, 0)),
        ("2024-01-01 22:30:00 UTC", at(2024, 1, 1, 22, 30, 0)),
        ("2024-01-01 22:30:00 EST", at(2024, 1, 2, 3, 30, 0)),
        ("2024-01-01 22:30:00 IST", at(2024, 1, 1, 20, 30, 0)),
        ("2024-01-01T", at(2024, 1, 1, 7, 0, 0)),
        ("Tue, 01 Jan 2024 10:00:00 GMT", at(2024, 1, 2, 10, 0, 0)),
        ("Jan 2024", at(2024, 1, 1, 0, 0, 0)),
        ("2024-W01-1", at(2024, 1, 1, 0, 0, 0)),
        ("2024-01-01T10:20:30.123Z", at(2024, 1, 1, 10, 20, 30) + 123),
        # The cases the Rust server's hand-written parser was tested on (PR #1759).
        ("2024-01-01 10pm", at(2024, 1, 1, 22, 0, 0)),
        ("2024-01-01 10 pm", at(2024, 1, 1, 22, 0, 0)),
        ("2024-01-01 10:00:00 am", at(2024, 1, 1, 10, 0, 0)),
        ("2024-01-01 12:00 PM", at(2024, 1, 1, 12, 0, 0)),
        ("2024-01-01 10:30 p.m.", at(2024, 1, 1, 22, 30, 0)),
        ("Jan 1 2024 10:00 PM", at(2024, 1, 1, 22, 0, 0)),
        ("10:00 PM 2024-01-01", at(2024, 1, 1, 22, 0, 0)),
        ("2024-01-01 10:00 PM +02:00", at(2024, 1, 1, 20, 0, 0)),
        ("2024-01-01 10:00 P", at(2024, 1, 1, 13, 0, 0)),
        ("2024-01-01 22:30:00 utc", at(2024, 1, 1, 22, 30, 0)),
        ("2024-01-01 22:30:00 GMT", at(2024, 1, 1, 22, 30, 0)),
        ("2024-01-01 22:30:00 PST", at(2024, 1, 2, 6, 30, 0)),
        ("2024-01-01 22:30:00 NDT", at(2024, 1, 2, 1, 0, 52)),
        ("2024-01-01 22:30:00 +02:00", at(2024, 1, 1, 20, 30, 0)),
        ("2024-01-01 22:30:00+0200", at(2024, 1, 1, 20, 30, 0)),
        ("2024-01-01 22:30:00 -05", at(2024, 1, 2, 3, 30, 0)),
        ("22:00 2024-01-01", at(2024, 1, 1, 22, 0, 0)),
        ("Jan 1 2024 22:00:00", at(2024, 1, 1, 22, 0, 0)),
        ("22:00:00 2024-01-01", at(2024, 1, 1, 22, 0, 0)),
        ("Mon, 01 Jan 2024 10:00:00 GMT", at(2024, 1, 1, 10, 0, 0)),
        ("Sun, 01 Jan 2024", at(2024, 1, 7, 0, 0, 0)),
        ("Tue Jan 1 10:00:00 2024", at(2024, 1, 2, 10, 0, 0)),
        ("2024 Jan", at(2024, 1, 1, 0, 0, 0)),
        ("Sept 2024", at(2024, 9, 1, 0, 0, 0)),
        ("Jan 1 10:00:00 2024", at(2024, 1, 1, 10, 0, 0)),
    ],
)
def test_values_match_mongod(text: str, expected: int) -> None:
    assert parse_ms(text) == expected


@pytest.mark.parametrize(
    "text",
    [
        "2024-01-01 22:30:00 SGT",
        "Jan, 2024",
        "2024-01-01 23:59:60",
        "2024-01-01T23:59:60",
        "2024-01-01 13:00 PM",
        "2024-01-01 0:30 am",
        "2024-01-01 10:00:30.5 pm",
        "2024-01-01T10:00 PM",
        "10:00 PM",
    ],
)
def test_refused_like_mongod(text: str) -> None:
    with pytest.raises(TimelibError):
        parse_ms(text)


@pytest.mark.parametrize(
    ("text", "fmt", "expected"),
    [
        ("2024-01-15", "%Y-%m-%d", at(2024, 1, 15, 0, 0, 0)),
        ("2024-01-15T10:30:45.1", "%Y-%m-%dT%H:%M:%S.%L", at(2024, 1, 15, 10, 30, 45) + 100),
        # %j is zero-based: day 15 is the 16th.
        ("2024-015", "%Y-%j", at(2024, 1, 16, 0, 0, 0)),
        ("2023-366", "%Y-%j", at(2024, 1, 2, 0, 0, 0)),
        ("2024-999", "%Y-%j", at(2026, 9, 26, 0, 0, 0)),
        ("2024-W03-1", "%G-W%V-%u", at(2024, 1, 15, 0, 0, 0)),
        ("2020-W53-7", "%G-W%V-%u", at(2021, 1, 3, 0, 0, 0)),
        ("2024", "%G", at(2024, 1, 1, 0, 0, 0)),
        ("2024-01-15 +0530", "%Y-%m-%d %z", at(2024, 1, 14, 18, 30, 0)),
        ("2024-01-15 10:30 EST", "%Y-%m-%d %H:%M %z", at(2024, 1, 15, 15, 30, 0)),
        # %Z is an offset in MINUTES.
        ("2024-01-15 10:30 -90", "%Y-%m-%d %H:%M %Z", at(2024, 1, 15, 12, 0, 0)),
        ("15 January 2024", "%d %B %Y", at(2024, 1, 15, 0, 0, 0)),
        ("24-01-15", "%Y-%m-%d", at(24, 1, 15, 0, 0, 0)),
        ("2024-01-15 1:05", "%Y-%m-%d %H:%M", at(2024, 1, 15, 1, 5, 0)),
    ],
)
def test_format_values_match_mongod(text: str, fmt: str, expected: int) -> None:
    assert format_ms(text, fmt) == expected


@pytest.mark.parametrize(
    ("text", "fmt", "message"),
    [
        (
            "2024",
            "%Y",
            'an incomplete date/time string has been found, with elements missing: "2024"',
        ),
        (
            "2024-13-01",
            "%Y-%m-%d",
            "Error parsing date string '2024-13-01'; 10: The parsed date was invalid '\0'",
        ),
        (
            "2024-01-15",
            "%Y/%m/%d",
            "Error parsing date string '2024-01-15'; 4: Format literal not found '-';"
            " 7: Format literal not found '-'",
        ),
        (
            "2024-01",
            "%Y-%m-%d",
            "Error parsing date string '2024-01'; 7: Not enough data available to satisfy"
            " format '\0'",
        ),
        (
            "abc",
            "%Y",
            "Error parsing date string 'abc'; 0: Unexpected data found. 'a';"
            " 0: A four digit year could not be found 'a'",
        ),
        (
            "2024-01-15 EST",
            "%Y-%m-%d %Z",
            "Error parsing date string '2024-01-15 EST'; 11: Invalid timezone offset in"
            " minutes 'E'; 11: Trailing data 'E'",
        ),
        (
            "2024-01-15 Europe/Dublin",
            "%Y-%m-%d %z",
            "Error parsing date string '2024-01-15 Europe/Dublin'; 11: passing a time zone"
            " identifier as part of the string is not allowed 'E'",
        ),
        (
            "2024 2024-W01-1",
            "%Y %G-W%V-%u",
            "Error parsing date string '2024 2024-W01-1'; 15: Mixing of ISO dates with natural"
            " dates is not allowed '\0'; 15: Mixing of ISO dates with natural dates is not"
            " allowed '\0'",
        ),
        (
            "015-2024",
            "%j-%Y",
            "Error parsing date string '015-2024'; 0: A 'day of year' can only come after a"
            " year has been found '0'",
        ),
    ],
)
def test_format_error_text_matches_mongod(text: str, fmt: str, message: str) -> None:
    assert format_err(text, fmt) == message


def test_format_validation_matches_mongod() -> None:
    assert validate_format("%Y-%m-%d %H:%M:%S.%L%z%Z%G%V%u%j%b%B%%") is None
    assert validate_format("%Y-%m-%d%") == (18535, "Unmatched '%' at end of format string")
    assert validate_format("%A") == (18536, "Invalid format character '%A' in format string")

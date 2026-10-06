"""A port of timelib's free-form date parser (``timelib_strtotime``), its
parse-from-format (``timelib_parse_from_format_with_map``) and the parts of
``timelib_update_ts`` they need -- the parser mongod 8.2.11 uses for
``$dateFromString`` (with and without a ``format``) and for ``$toDate`` /
``$convert`` of a string.

Source: timelib 2022.13 (``parse_date.re``, ``tm2unixtime.c``, ``dow.c``), the
copy vendored by mongod at ``src/third_party/timelib/dist``, and mongod's own
wrapper ``TimeZoneDatabase::fromString`` (``date_time_support.cpp``), which
turns timelib's errors into the message a client sees.

Copyright (c) 2015-2023 Derick Rethans, (c) 2017-2019,2021 MongoDB, Inc.
MIT License (``LICENSE-timelib.rst`` in this package). Ported for SecantusDB.

The port is deliberately literal -- same names, same order of operations, same
quirks (a ``"T"`` alone is military zone T, a leading weekday is a relative
jump) -- because mongod's answer, including the position and character in
every error, falls out of exactly those steps. It is pure Python: the Python
server has no Rust in its request path. Its exemplar is mongod; the Rust
server's port (``crates/secantus-core/src/timelib``) is the same translation,
not an authority.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from secantus.timelib.consts import (
    ERR_TZID_NOT_FOUND,
    I64_MAX,
    I64_MIN,
    UNSET,
    ZONETYPE_ABBR,
    ZONETYPE_OFFSET,
)

__all__ = [
    "UNSET",
    "ZONETYPE_ABBR",
    "ZONETYPE_OFFSET",
    "Message",
    "Parsed",
    "Relative",
    "Time",
    "millis",
    "mongo_parse",
    "mongo_parse_format",
    "update_ts",
    "validate_format",
]


@dataclass
class Relative:
    """``timelib_rel_time``, the fields the parser and ``update_ts`` use."""

    y: int = 0
    m: int = 0
    d: int = 0
    h: int = 0
    i: int = 0
    s: int = 0
    us: int = 0
    weekday: int = 0
    weekday_behavior: int = 0
    first_last_day_of: int = 0
    special_type: int = 0
    special_amount: int = 0
    have_weekday_relative: bool = False
    have_special_relative: bool = False


@dataclass
class Time:
    """``timelib_time``, the fields the parser and ``update_ts`` use."""

    y: int = UNSET
    m: int = UNSET
    d: int = UNSET
    h: int = UNSET
    i: int = UNSET
    s: int = UNSET
    us: int = UNSET
    z: int = UNSET
    dst: int = UNSET
    zone_type: int = 0
    #: Upper-cased, as ``timelib_time_tz_abbr_update`` stores it.
    tz_abbr: str = ""
    relative: Relative = field(default_factory=Relative)
    have_time: int = 0
    have_date: bool = False
    have_zone: int = 0
    have_relative: bool = False
    #: Seconds since the epoch, set by ``update_ts``.
    sse: int = 0


@dataclass
class Message:
    """One entry of timelib's error or warning list."""

    code: int
    position: int
    character: int
    message: str


@dataclass
class Parsed:
    """What ``timelib_strtotime`` returns."""

    time: Time
    errors: list[Message]
    warnings: list[Message]


# Imported after the types they use.
from secantus.timelib.format import parse_from_format, validate_format  # noqa: E402
from secantus.timelib.scan import strtotime  # noqa: E402
from secantus.timelib.update import update_ts  # noqa: E402


class TimelibError(ValueError):
    """mongod's ``fromString`` refused the string; ``str(exc)`` is its message."""


def mongo_parse(text: str) -> Time:
    """mongod's ``TimeZoneDatabase::fromString`` with no ``format``, up to (not
    including) the time-zone argument: parse, turn any error or warning into
    mongod's message, default a missing time of day, and refuse a string
    missing any date or time part. Raises :class:`TimelibError`."""
    return _from_string(text, strtotime(text.encode("utf-8", "surrogatepass")))


def mongo_parse_format(text: str, fmt: str) -> Time:
    """The same with a ``format``: ``timelib_parse_from_format_with_map`` under
    mongod's map. The caller has already validated the format with
    :func:`validate_format`."""
    return _from_string(
        text,
        parse_from_format(
            fmt.encode("utf-8", "surrogatepass"), text.encode("utf-8", "surrogatepass")
        ),
    )


def _from_string(text: str, parsed: Parsed) -> Time:
    """``TimeZoneDatabase::fromString`` after the timelib call."""
    if parsed.errors or parsed.warnings:
        sb = f"Error parsing date string '{text}'"
        for e in parsed.errors:
            sb += f"; {e.position}: "
            # mongod never makes zone identifiers available, so it rewrites
            # this one message.
            if e.code == ERR_TZID_NOT_FOUND:
                sb += "passing a time zone identifier as part of the string is not allowed"
            else:
                sb += e.message
            sb += f" '{chr(e.character)}'"
        for w in parsed.warnings:
            sb += f"; {w.position}: {w.message} '{chr(w.character)}'"
        raise TimelibError(sb)
    t = parsed.time
    # A fully missing time of day is midnight, which lets `%Y-%m-%d` through.
    if t.h == UNSET and t.i == UNSET and t.s == UNSET:
        t.h = t.i = t.s = t.us = 0
    if UNSET in (t.y, t.m, t.d, t.h, t.i, t.s):
        raise TimelibError(
            f'an incomplete date/time string has been found, with elements missing: "{text}"'
        )
    return t


def millis(t: Time) -> int | None:
    """Milliseconds since the epoch of a time ``update_ts`` has resolved, as
    mongod computes it: ``Seconds(sse) + Microseconds(us)``, converted to
    milliseconds (truncating toward zero). The seconds are widened to
    MICROseconds first, so a value beyond about +-292,000 years overflows
    there -- mongod's 159 -- even when it would fit in milliseconds. ``None``
    on that overflow; there is no other bound."""
    us = 0 if t.us == UNSET else t.us
    micros = t.sse * 1_000_000
    if not I64_MIN <= micros <= I64_MAX:
        return None
    micros += us
    if not I64_MIN <= micros <= I64_MAX:
        return None
    q = abs(micros) // 1000
    return q if micros >= 0 else -q

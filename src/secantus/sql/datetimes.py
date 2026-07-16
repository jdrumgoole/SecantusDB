"""Postgres date-only / time-only types: ``date`` / ``time`` / ``timetz``.

BSON has no date-only or time-only value, and ``datetime.date`` /
``datetime.time`` are not BSON-encodable — so these types are stored as their
canonical text (``date`` as ``YYYY-MM-DD``, ``time`` as ``HH:MM:SS[.ffffff]``,
``timetz`` as ``HH:MM:SS[.ffffff][+HH:MM]``). ISO text orders and compares the
same as the underlying value, so equality / ``ORDER BY`` lower to a Mongo filter,
and a text value is distinguishable at evaluation time from a ``datetime``
(``timestamptz``). ``secantus.sql`` ``scalar`` / ``typemap`` / ``planner`` wire
it into the SQL surface.

Out of scope: microsecond-precision rounding to a declared ``time(p)`` scale,
and time-zone conversion beyond preserving the literal's offset.
"""

from __future__ import annotations

import datetime as _dt
import re
from typing import Any

_SHORT_OFFSET_RE = re.compile(r"([+-]\d{2})(\d{2})?$")


def parse_iso_datetime(v: Any) -> _dt.datetime:
    """``datetime.fromisoformat`` with a Python 3.10 compatibility net.

    3.11+ parses a trailing ``Z`` and Postgres's short UTC offsets (``+00`` /
    ``+0000``) directly; 3.10 rejects them — and PG text rendering emits the
    short form, so timestamptz literals and round-tripped wire text must parse
    here. Try the fast path first (a no-op passthrough on 3.11+), and only on
    failure widen a trailing short offset to ``+HH:MM``.
    """
    s = str(v).strip().replace("Z", "+00:00")
    try:
        return _dt.datetime.fromisoformat(s)
    except ValueError:
        m = _SHORT_OFFSET_RE.search(s)
        if m:
            fixed = s[: m.start()] + m.group(1) + ":" + (m.group(2) or "00")
            return _dt.datetime.fromisoformat(fixed)
        raise


_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_TIME_RE = re.compile(r"^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?$")
_TIMETZ_RE = re.compile(
    r"^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?\s*([+-]\d{1,2}(?::?\d{2})?)$"
)


class DateTimeError(ValueError):
    """A malformed date / time / timetz literal."""


def parse_date(value: Any) -> str:
    """Canonicalise a ``date`` to ``YYYY-MM-DD``. Accepts a date/datetime object or
    an ISO date string (a datetime string is truncated to its date)."""
    if isinstance(value, _dt.datetime):
        return value.date().isoformat()
    if isinstance(value, _dt.date):
        return value.isoformat()
    s = str(value).strip()
    try:
        return _dt.date.fromisoformat(s[:10]).isoformat()
    except ValueError as e:
        raise DateTimeError(f"invalid date value: {value!r}") from e


def _fmt_time(hh: int, mm: int, ss: int, frac: str) -> str:
    base = f"{hh:02d}:{mm:02d}:{ss:02d}"
    if frac:
        micros = (frac + "000000")[:6].rstrip("0")
        if micros:
            base += "." + micros
    return base


def parse_time(value: Any) -> str:
    """Canonicalise a ``time`` to ``HH:MM:SS[.ffffff]``."""
    if isinstance(value, _dt.datetime):
        value = value.time()
    if isinstance(value, _dt.time):
        s = value.isoformat()
        return parse_time(s)
    m = _TIME_RE.match(str(value).strip())
    if m is None:
        raise DateTimeError(f"invalid time value: {value!r}")
    hh, mm = int(m.group(1)), int(m.group(2))
    ss = int(m.group(3)) if m.group(3) else 0
    return _fmt_time(hh, mm, ss, m.group(4) or "")


def parse_timetz(value: Any) -> str:
    """Canonicalise a ``timetz`` to ``HH:MM:SS[.ffffff]+HH:MM`` (offset preserved)."""
    m = _TIMETZ_RE.match(str(value).strip())
    if m is None:
        # No offset given — treat as UTC (+00:00), matching a bare ``timetz`` cast.
        return parse_time(value) + "+00:00"
    hh, mm = int(m.group(1)), int(m.group(2))
    ss = int(m.group(3)) if m.group(3) else 0
    base = _fmt_time(hh, mm, ss, m.group(4) or "")
    return base + _normalize_offset(m.group(5))


def _normalize_offset(off: str) -> str:
    sign = off[0]
    rest = off[1:].replace(":", "")
    hours = int(rest[:2])
    minutes = int(rest[2:4]) if len(rest) > 2 else 0
    return f"{sign}{hours:02d}:{minutes:02d}"


def render_date(value: Any) -> str:
    return parse_date(value)


def is_date_value(v: Any) -> bool:
    """Whether ``v`` is a stored ``date`` — an ISO ``YYYY-MM-DD`` string or a
    ``datetime.date`` that is not a ``datetime`` (so it is distinguishable from a
    ``timestamptz``)."""
    if isinstance(v, _dt.date) and not isinstance(v, _dt.datetime):
        return True
    return isinstance(v, str) and bool(_DATE_RE.match(v))


def is_time_value(v: Any) -> bool:
    return isinstance(v, str) and _TIME_RE.match(v) is not None


def to_date_obj(v: Any) -> _dt.date:
    if isinstance(v, _dt.datetime):
        return v.date()
    if isinstance(v, _dt.date):
        return v
    return _dt.date.fromisoformat(str(v)[:10])


def to_time_obj(v: Any) -> _dt.time:
    return _dt.time.fromisoformat(parse_time(v))


def date_sub_date(a: Any, b: Any) -> int:
    """``date - date`` -> the integer number of days (Postgres semantics)."""
    return (to_date_obj(a) - to_date_obj(b)).days


def date_add_days(a: Any, n: int) -> str:
    """``date + int`` / ``date - int`` -> a ``date`` shifted by ``n`` days."""
    return (to_date_obj(a) + _dt.timedelta(days=int(n))).isoformat()

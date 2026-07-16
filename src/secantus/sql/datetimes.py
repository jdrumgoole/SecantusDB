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
# A trailing UTC offset in any PG-accepted looseness: ``+2``, ``+02``, ``+0230``,
# ``+02:30``, ``+01:02:03`` (seconds-bearing).
_LOOSE_OFFSET_RE = re.compile(r"([+-])(\d{1,2})(?::?(\d{2}))?(?::(\d{2}))?$")
# Loose date prefix: PG accepts non-padded fields (``2000-1-1``).
_LOOSE_DATE_RE = re.compile(r"^(\d{4,})-(\d{1,2})-(\d{1,2})")

#: PG's special datetime input values that we carry as text sentinels.
INFINITY = "infinity"
NEG_INFINITY = "-infinity"


def datetime_sentinel(v: Any) -> str | None:
    """``infinity`` / ``-infinity`` for PG's special input spellings, else None.
    Sentinels flow through storage and rendering as text; the binary encoders
    map them onto Postgres' int64 min/max wire values."""
    s = str(v).strip().lower()
    if s in ("infinity", "+infinity"):
        return INFINITY
    if s == "-infinity":
        return NEG_INFINITY
    return None


def _widen(s: str) -> str:
    """Rewrite PG-accepted loose spellings into ``fromisoformat`` shape: pad
    date and hour fields, widen a short/seconds-bearing trailing offset."""
    m = _LOOSE_DATE_RE.match(s)
    if m and (len(m.group(2)) == 1 or len(m.group(3)) == 1):
        s = f"{m.group(1)}-{int(m.group(2)):02d}-{int(m.group(3)):02d}" + s[m.end() :]
    # Pad a single-digit hour (``… 1:12:32`` -> ``… 01:12:32``).
    m = re.match(r"^(\d{4,}-\d{2}-\d{2}[ T])(\d):", s)
    if m:
        s = f"{m.group(1)}0{m.group(2)}" + s[m.end() - 1 :]
    m = _LOOSE_OFFSET_RE.search(s)
    if m and m.start() > 0 and s[: m.start()].rstrip()[-1:].isdigit():
        secs = m.group(4)
        off = f"{m.group(1)}{int(m.group(2)):02d}:{m.group(3) or '00'}"
        if secs:
            off += f":{secs}"
        s = s[: m.start()].rstrip() + off
    return s


_TZ_NUM_RE = re.compile(r"^([+-]?)(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?$")


def tzinfo_for_setting(setting: str) -> _dt.tzinfo:
    """The tzinfo for a ``TimeZone`` GUC value. Numeric-offset strings follow
    the POSIX sign convention Postgres applies to them: ``'-02:00'`` means
    UTC+2 (display offset +02:00). Zone names resolve via zoneinfo; anything
    unresolvable falls back to UTC."""
    s = (setting or "").strip().strip("'\"")
    if not s or s.lower() in ("utc", "gmt"):
        return _dt.timezone.utc
    m = _TZ_NUM_RE.match(s)
    if m:
        seconds = int(m.group(2)) * 3600 + int(m.group(3) or 0) * 60 + int(m.group(4) or 0)
        # POSIX sign convention: an unsigned or ``+`` zone string is west of
        # Greenwich (``'12:00'`` / ``'+12'`` mean UTC-12); ``-`` is east.
        sign = 1 if m.group(1) == "-" else -1
        return _dt.timezone(_dt.timedelta(seconds=sign * seconds))
    try:
        import zoneinfo

        return zoneinfo.ZoneInfo(s)
    except Exception:  # noqa: BLE001 — unknown zone name: PG errors, we soften to UTC
        return _dt.timezone.utc


def session_tzinfo(session: Any) -> _dt.tzinfo:
    """The connection's ``TimeZone`` GUC as a tzinfo (UTC when unset)."""
    getter = getattr(session, "get_setting", None)
    if getter is None:
        return _dt.timezone.utc
    return tzinfo_for_setting(getter("TimeZone"))


def session_offset_text(session: Any) -> str:
    """The session TimeZone's current UTC offset as ``±HH:MM[:SS]`` — the
    offset an offset-less ``timetz`` literal takes."""
    tz = session_tzinfo(session)
    off = _dt.datetime.now(tz).utcoffset() or _dt.timedelta(0)
    total = int(off.total_seconds())
    sign = "+" if total >= 0 else "-"
    total = abs(total)
    hh, rem = divmod(total, 3600)
    mm, ss = divmod(rem, 60)
    out = f"{sign}{hh:02d}:{mm:02d}"
    if ss:
        out += f":{ss:02d}"
    return out


_DAYS_BEFORE_MONTH = (0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334)


def _is_leap(y: int) -> bool:
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)


def gregorian_ordinal(y: int, m: int, d: int) -> int:
    """Proleptic-Gregorian day number with 0001-01-01 = 1 (Python's ``date
    ordinal``), valid for astronomical years outside ``datetime.date``'s
    [1, 9999] range — BC years are y <= 0, far-future years > 9999. Floor
    division keeps the leap arithmetic correct for negative years."""
    py = y - 1
    n = 365 * py + py // 4 - py // 100 + py // 400
    n += _DAYS_BEFORE_MONTH[m] + (1 if m > 2 and _is_leap(y) else 0)
    return n + d


def ordinal_to_gregorian(n: int) -> tuple[int, int, int]:
    """Inverse of ``gregorian_ordinal`` for any day number (binary search on the
    year, then month walk)."""
    # A year is ~365.2425 days; bracket then correct.
    y = n * 400 // 146097 + 1
    while gregorian_ordinal(y + 1, 1, 1) <= n:
        y += 1
    while gregorian_ordinal(y, 1, 1) > n:
        y -= 1
    doy = n - gregorian_ordinal(y, 1, 1)  # 0-based day of year
    leap = _is_leap(y)
    for m in range(12, 0, -1):
        before = _DAYS_BEFORE_MONTH[m] + (1 if m > 2 and leap else 0)
        if doy >= before:
            return y, m, doy - before + 1
    return y, 1, doy + 1


# ``10000-01-01 12:00`` / ``1000-01-01 12:00 BC`` — PG-range timestamps beyond
# Python's datetime limits, carried through storage and text output verbatim.
_WIDE_TS_RE = re.compile(
    r"^(\d{4,7})-(\d{1,2})-(\d{1,2})"
    r"(?:[ T](\d{1,2}):(\d{2})(?::(\d{2})(?:\.(\d+))?)?)?"
    r"\s*(?:([+-])(\d{1,2})(?::?(\d{2}))?)?"
    r"(\s+BC)?$",
    re.IGNORECASE,
)


def wide_timestamp_text(v: Any) -> str | None:
    """Canonical text for a PG-valid timestamp outside Python's datetime range
    (year > 9999 or a BC date), or None when the value is in-range / malformed.
    The canonical form is ``YYYY-MM-DD HH:MM:SS[.ffffff][+TZ][ BC]``."""
    m = _WIDE_TS_RE.match(str(v).strip())
    if m is None:
        return None
    y = int(m.group(1))
    bc = bool(m.group(11))
    if y <= 9999 and not bc:
        return None
    mo, d = int(m.group(2)), int(m.group(3))
    hh = int(m.group(4) or 0)
    mi = int(m.group(5) or 0)
    ss = int(m.group(6) or 0)
    frac = (m.group(7) or "").ljust(6, "0")[:6].rstrip("0")
    text = f"{y:04d}-{mo:02d}-{d:02d} {hh:02d}:{mi:02d}:{ss:02d}"
    if frac:
        text += f".{frac}"
    if m.group(8):
        off = f"{m.group(8)}{int(m.group(9)):02d}"
        if m.group(10):
            off += f":{m.group(10)}"
        text += off
    if bc:
        text += " BC"
    return text


def wide_timestamp_micros(text: str) -> int:
    """PG binary wire value (µs from 2000-01-01) for a ``wide_timestamp_text``
    canonical string — used by the binary encoder so a client sees the true
    out-of-range instant and raises its own range error."""
    m = _WIDE_TS_RE.match(text.strip())
    if m is None:
        raise ValueError(f"not a wide timestamp: {text!r}")
    y = int(m.group(1))
    if m.group(11):  # BC: year N BC is astronomical year 1-N
        y = 1 - y
    days = gregorian_ordinal(y, int(m.group(2)), int(m.group(3))) - gregorian_ordinal(2000, 1, 1)
    micros = (
        int(m.group(4) or 0) * 3600 + int(m.group(5) or 0) * 60 + int(m.group(6) or 0)
    ) * 1_000_000
    micros += int((m.group(7) or "").ljust(6, "0")[:6])
    if m.group(8):
        off = int(m.group(9)) * 3600 + int(m.group(10) or 0) * 60
        micros -= off * 1_000_000 * (1 if m.group(8) == "+" else -1)
    return days * 86_400_000_000 + micros


def wide_date_days(text: str) -> int:
    """PG binary wire value (days from 2000-01-01) for an out-of-range date
    text (``10000-01-01`` / ``1000-01-01 BC``)."""
    return wide_timestamp_micros(text) // 86_400_000_000


def parse_iso_datetime(v: Any) -> _dt.datetime:
    """``datetime.fromisoformat`` widened for PG-accepted spellings.

    Postgres accepts non-padded date fields (``2000-1-1``), single-digit and
    seconds-bearing offsets (``+2`` / ``+01:02:03``), the short UTC offsets its
    own text rendering emits (``+00`` / ``+0000``), a trailing ``Z``, and the
    special value ``epoch``. Try the fast path first, then widen.
    """
    s = str(v).strip().replace("Z", "+00:00")
    if s.lower() == "epoch":
        return _dt.datetime(1970, 1, 1, tzinfo=_dt.timezone.utc)
    try:
        return _dt.datetime.fromisoformat(s)
    except ValueError:
        pass
    m = _SHORT_OFFSET_RE.search(s)
    if m:
        fixed = s[: m.start()] + m.group(1) + ":" + (m.group(2) or "00")
        try:
            return _dt.datetime.fromisoformat(fixed)
        except ValueError:
            pass
    return _dt.datetime.fromisoformat(_widen(s))


_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_TIME_RE = re.compile(r"^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?$")
_TIMETZ_RE = re.compile(
    r"^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?\s*([+-]\d{1,2}(?::?\d{2})?)$"
)


class DateTimeError(ValueError):
    """A malformed date / time / timetz literal."""


_BC_DATE_RE = re.compile(r"^(\d{1,7})-(\d{1,2})-(\d{1,2})\s+BC$", re.IGNORECASE)
_WIDE_DATE_RE = re.compile(r"^(\d{4,7})-(\d{1,2})-(\d{1,2})$")


def parse_date(value: Any) -> str:
    """Canonicalise a ``date`` to ``YYYY-MM-DD``. Accepts a date/datetime object
    or an ISO date string (a datetime string is truncated to its date). PG's
    special values (``infinity`` / ``epoch``), BC dates, and dates beyond
    Python's year-9999 ceiling canonicalise to text — PG's range is far wider
    than ``datetime.date``'s, and silently truncating (the old ``s[:10]``)
    turned ``1000-01-01 BC`` into 1000-01-01 AD."""
    if isinstance(value, _dt.datetime):
        return value.date().isoformat()
    if isinstance(value, _dt.date):
        return value.isoformat()
    s = str(value).strip()
    sentinel = datetime_sentinel(s)
    if sentinel is not None:
        return sentinel
    if s.lower() == "epoch":
        return "1970-01-01"
    m = _BC_DATE_RE.match(s)
    if m:
        y, mo, d = int(m.group(1)), int(m.group(2)), int(m.group(3))
        return f"{y:04d}-{mo:02d}-{d:02d} BC"
    m = _WIDE_DATE_RE.match(s)
    if m and int(m.group(1)) > 9999:
        y, mo, d = int(m.group(1)), int(m.group(2)), int(m.group(3))
        return f"{y:04d}-{mo:02d}-{d:02d}"
    try:
        # Strict on the date part, but accept a trailing datetime tail.
        head = s[:10] if len(s) > 10 and (s[10:11] in (" ", "T")) else s
        return _dt.date.fromisoformat(_widen_date(head)).isoformat()
    except ValueError:
        pass
    try:
        # A full (possibly loose) datetime string — truncate to its date.
        return parse_iso_datetime(s).date().isoformat()
    except ValueError as e:
        raise DateTimeError(f"invalid date value: {value!r}") from e


def _widen_date(s: str) -> str:
    m = re.match(r"^(\d{4})-(\d{1,2})-(\d{1,2})$", s.strip())
    if m:
        return f"{m.group(1)}-{int(m.group(2)):02d}-{int(m.group(3)):02d}"
    return s.strip()


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


def parse_timetz(value: Any, default_offset: str = "+00:00") -> str:
    """Canonicalise a ``timetz`` to ``HH:MM:SS[.ffffff]+HH:MM`` (offset preserved).
    A trailing ``Z`` is the UTC offset; an offset-less literal takes
    ``default_offset`` (the session TimeZone's offset at a real server)."""
    s = str(value).strip()
    if s.endswith(("Z", "z")):
        s = s[:-1] + "+00:00"
    m = _TIMETZ_RE.match(s)
    if m is None:
        return parse_time(s) + default_offset
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
    """``date + int`` / ``date - int`` -> a ``date`` shifted by ``n`` days.
    A result outside Python's date range (PG's range is far wider) is computed
    with proleptic ordinal math and returned as text (``10000-01-01``)."""
    try:
        return (to_date_obj(a) + _dt.timedelta(days=int(n))).isoformat()
    except OverflowError:
        base = to_date_obj(a)
        y, m, d = ordinal_to_gregorian(gregorian_ordinal(base.year, base.month, base.day) + int(n))
        if y <= 0:
            return f"{1 - y:04d}-{m:02d}-{d:02d} BC"
        return f"{y:04d}-{m:02d}-{d:02d}"

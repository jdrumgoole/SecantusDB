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


#: A bare ``YYYY-MM-DD`` with nothing after it.
_DATE_ONLY_RE = re.compile(r"^\d{4,}-\d{1,2}-\d{1,2}$")


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
        head = s[: m.start()].rstrip()
        if _DATE_ONLY_RE.match(head):
            # ``1950-02-07 -05`` — a date with an offset and no time, which is
            # what a JDBC setDate with a Calendar sends. Dropping the space
            # left ``1950-02-07-05:00``, which fromisoformat reads as the TIME
            # 05:00 rather than an offset: the value silently became five in
            # the morning, naive, and a timestamp column stored it that way.
            # Postgres reads the implicit midnight, so spell it out.
            head += " 00:00:00"
        s = head + off
    return s


_TZ_NUM_RE = re.compile(r"^([+-]?)(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?$")
#: ``GMT+13`` / ``UTC-5`` — a zone name with a POSIX offset suffix.
_TZ_PREFIXED_RE = re.compile(r"(?i)^(?:GMT|UTC)([+-])(\d{1,2})(?::(\d{2}))?(?::(\d{2}))?$")


def tzinfo_for_setting(setting: str) -> _dt.tzinfo:
    """The tzinfo for a ``TimeZone`` GUC value. Numeric-offset strings follow
    the POSIX sign convention Postgres applies to them: ``'-02:00'`` means
    UTC+2 (display offset +02:00). Zone names resolve via zoneinfo; anything
    unresolvable falls back to UTC."""
    s = (setting or "").strip().strip("'\"")
    if not s or s.lower() in ("utc", "gmt"):
        return _dt.timezone.utc
    # ``GMT+13`` / ``UTC-5`` — Postgres accepts the prefixed POSIX spelling and
    # keeps the POSIX sign, so GMT+13 is UTC-13. zoneinfo spells those
    # ``Etc/GMT±N`` with the same inverted sign, so the suffix carries over
    # unchanged. Checked against PostgreSQL 14.13, which renders GMT+13 as -13.
    pm = _TZ_PREFIXED_RE.match(s)
    if pm:
        # POSIX sign convention, so GMT+13 is UTC-13 (checked against
        # PostgreSQL 14.13, which renders it as -13; GMT+3:30 is UTC-03:30 —
        # pgjdbc's halfHourTimezone test drives exactly that spelling). Built
        # as a fixed offset rather than via zoneinfo's ``Etc/GMT±N``, which
        # stops at ±12 while Postgres accepts more (and has no half-hour
        # entries at all).
        seconds = int(pm.group(2)) * 3600 + int(pm.group(3) or 0) * 60 + int(pm.group(4) or 0)
        return _dt.timezone(_dt.timedelta(seconds=-seconds if pm.group(1) == "+" else seconds))
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
# PG's datetime input is field-order flexible: the era marker may precede or
# follow the zone offset (``0101-01-01 00:00 BC +00`` — which is what pgjdbc
# emits — as well as PG's own output order ``…+00 BC``). Named groups so the
# two era slots collapse to one.
_WIDE_TS_RE = re.compile(
    r"^(?P<y>\d{1,7})-(?P<mo>\d{1,2})-(?P<d>\d{1,2})"
    r"(?:[ T](?P<hh>\d{1,2}):(?P<mi>\d{2})(?::(?P<ss>\d{2})(?:\.(?P<frac>\d+))?)?)?"
    r"(?P<bc1>\s+BC)?"
    r"\s*(?:(?P<sign>[+-])(?P<oh>\d{1,2})(?::?(?P<om>\d{2}))?)?"
    r"(?P<bc2>\s+BC)?$",
    re.IGNORECASE,
)


def wide_timestamp_text(
    v: Any, *, drop_offset: bool = False, default_offset: str | None = None
) -> str | None:
    """Canonical text for a PG-valid timestamp outside Python's datetime range
    (year > 9999 or a BC date), or None when the value is in-range / malformed.
    The canonical form is ``YYYY-MM-DD HH:MM:SS[.ffffff][+TZ][ BC]``.

    ``drop_offset`` discards any offset the input carried, which is what a
    ``timestamp`` (without time zone) column does with one — Postgres keeps the
    wall-clock fields and forgets the zone. Rendering it back left a BC value
    reading ``0101-01-01 00:00:00+00 BC`` where Postgres writes
    ``0101-01-01 00:00:00 BC``.

    ``default_offset`` (``±HH:MM``) is stamped onto an input that carried NO
    offset — a timestamptz literal without a zone is wall clock in the session
    zone, and ``wide_timestamp_micros`` recovers the true instant from the
    stamped text (a BC date read back through a non-UTC session shifted a day
    without it — pgjdbc's DateTest GMT-N matrix).
    """
    m = _WIDE_TS_RE.match(str(v).strip())
    if m is None:
        return None
    y = int(m.group("y"))
    bc = bool(m.group("bc1") or m.group("bc2"))
    if y <= 9999 and not bc:
        return None
    mo, d = int(m.group("mo")), int(m.group("d"))
    hh = int(m.group("hh") or 0)
    mi = int(m.group("mi") or 0)
    ss = int(m.group("ss") or 0)
    frac = (m.group("frac") or "").ljust(6, "0")[:6].rstrip("0")
    text = f"{y:04d}-{mo:02d}-{d:02d} {hh:02d}:{mi:02d}:{ss:02d}"
    if frac:
        text += f".{frac}"
    if m.group("sign") and not drop_offset:
        off = f"{m.group('sign')}{int(m.group('oh')):02d}"
        if m.group("om"):
            off += f":{m.group('om')}"
        text += off
    elif default_offset is not None and not drop_offset and default_offset != "+00:00":
        text += default_offset
    if bc:
        # PG renders the era last, after any zone offset.
        text += " BC"
    return text


def wide_timestamp_micros(text: str) -> int:
    """PG binary wire value (µs from 2000-01-01) for a ``wide_timestamp_text``
    canonical string — used by the binary encoder so a client sees the true
    out-of-range instant and raises its own range error."""
    m = _WIDE_TS_RE.match(text.strip())
    if m is None:
        raise ValueError(f"not a wide timestamp: {text!r}")
    y = int(m.group("y"))
    if m.group("bc1") or m.group("bc2"):  # BC: year N BC is astronomical year 1-N
        y = 1 - y
    days = gregorian_ordinal(y, int(m.group("mo")), int(m.group("d"))) - gregorian_ordinal(
        2000, 1, 1
    )
    micros = (
        int(m.group("hh") or 0) * 3600 + int(m.group("mi") or 0) * 60 + int(m.group("ss") or 0)
    ) * 1_000_000
    micros += int((m.group("frac") or "").ljust(6, "0")[:6])
    if m.group("sign"):
        off = int(m.group("oh")) * 3600 + int(m.group("om") or 0) * 60
        micros -= off * 1_000_000 * (1 if m.group("sign") == "+" else -1)
    return days * 86_400_000_000 + micros


def wide_date_days(text: str) -> int:
    """PG binary wire value (days from 2000-01-01) for an out-of-range date
    text (``10000-01-01`` / ``1000-01-01 BC``)."""
    return wide_timestamp_micros(text) // 86_400_000_000


#: A ``:60`` seconds field — a leap second. Postgres accepts exactly ``:60``
#: with no fractional part and rolls it forward to the next minute; ``:60.5``
#: and ``:61`` are errors (checked against PostgreSQL 14.13).
_LEAP_SECOND_RE = re.compile(r"(?P<hm>\d{1,2}:\d{2}):60(?P<frac>[.,]\d+)?")


def _strip_leap_second(text: str) -> tuple[str, bool]:
    """``(text_with_59_seconds, rolls_forward)``.

    Python's ``datetime`` has no room for a leap second — it rejects second 60
    outright — so the value is parsed as :59 and a second added back by the
    caller. A fractional leap second is out of range in Postgres too, so it is
    left alone here and fails as an ordinary bad timestamp.
    """
    m = _LEAP_SECOND_RE.search(text)
    if m is None or m.group("frac"):
        return text, False
    return text[: m.start()] + m.group("hm") + ":59" + text[m.end() :], True


_SLASH_DATE_RE = re.compile(r"^(\d{1,2})/(\d{1,2})/(\d{1,4})(?:\s+(.*))?$")


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
    if "/" in s:
        # Slash-format date per the session's DateStyle field order:
        # '8/10/7777' is Aug 10 under MDY (the default), Oct 8 under DMY —
        # PG's non-ISO input form (pgjdbc's ResultSetTest.testTimestamp).
        m = _SLASH_DATE_RE.match(s)
        if m:
            a, b, year, rest = m.groups()
            from secantus.sql.typemap import _render_session

            session = _render_session.get()
            style = (session.get_setting("DateStyle") if session is not None else "") or ""
            dmy = "DMY" in style.upper()
            month, day = (b, a) if dmy else (a, b)
            iso = f"{int(year):04d}-{int(month):02d}-{int(day):02d}"
            return parse_iso_datetime(iso + ((" " + rest) if rest else ""))
    s, leap = _strip_leap_second(s)
    if leap:
        # Parsed as :59 and rolled forward, which is what Postgres does with a
        # leap second: '2015-06-30 23:59:60' is 2015-07-01 00:00:00, carrying
        # across the minute, day and year boundaries as needed.
        return parse_iso_datetime(s) + _dt.timedelta(seconds=1)
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
    r"^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?\s*([+-]\d{1,2}(?::?\d{2})?(?::\d{2})?)$"
)


class DateTimeError(ValueError):
    """A malformed date / time / timetz literal."""


# A BC date may carry a trailing zone offset (pgjdbc sends
# ``0101-01-01 BC +00`` for a date parameter); the offset is irrelevant
# to a date and ignored.
_BC_DATE_RE = re.compile(
    r"^(\d{1,7})-(\d{1,2})-(\d{1,2})\s+BC(?:\s*[+-]\d{1,2}(?::?\d{2})?)?$",
    re.IGNORECASE,
)
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
    # A BC / out-of-range value carrying a TIME part (``0101-01-01 00:00:00 BC
    # +00`` — pgjdbc binds a date parameter that way): keep the era, drop the
    # time. Falling through to the datetime parsers below silently lost the
    # BC and turned it into an AD date.
    m = _WIDE_TS_RE.match(s)
    if m and (m.group("bc1") or m.group("bc2") or int(m.group("y")) > 9999):
        y, mo, d = int(m.group("y")), int(m.group("mo")), int(m.group("d"))
        era = " BC" if (m.group("bc1") or m.group("bc2")) else ""
        return f"{y:04d}-{mo:02d}-{d:02d}{era}"
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


# A trailing zone offset / era marker on a time-of-day string.
_TZ_SUFFIX_RE = re.compile(r"(?i)\s*(?:[+-]\d{1,2}(?::?\d{2})?|Z)?(?:\s+BC)?\s*$")
_TS_TIME_TAIL_RE = re.compile(
    r"^\d{1,7}-\d{1,2}-\d{1,2}[ T](?P<time>\d{1,2}:\d{2}(?::\d{2})?(?:\.\d+)?)"
)


def _time_part_of_timestamp(text: str) -> str | None:
    """The time-of-day of a full timestamp string, or None when ``text`` isn't
    one. Any zone offset / era suffix is left for the caller's own matcher."""
    m = _TS_TIME_TAIL_RE.match(text)
    if m is None:
        return None
    return text[m.start("time") :]


def parse_time(value: Any) -> str:
    """Canonicalise a ``time`` to ``HH:MM:SS[.ffffff]``."""
    if isinstance(value, _dt.datetime):
        value = value.time()
    if isinstance(value, _dt.time):
        s = value.isoformat()
        return parse_time(s)
    text = str(value).strip()
    m = _TIME_RE.match(text)
    if m is None:
        # ``time`` ignores a zone offset the literal carries (verified against
        # PostgreSQL 14.13: ``'13:06:18+02'::time`` -> ``13:06:18``).
        m = _TIME_RE.match(_TZ_SUFFIX_RE.sub("", text).strip())
    if m is None:
        # PG's time input accepts a full timestamp and keeps only the
        # time-of-day (verified against PostgreSQL 14.13:
        # ``'2026-08-01 13:06:18.09+00'::timetz`` -> ``13:06:18.09+00``).
        # pgjdbc stores CURRENT_TIMESTAMP into a time column this way.
        tail = _time_part_of_timestamp(text)
        if tail is not None:
            # ``time`` (no zone) drops any offset / era the timestamp carried.
            m = _TIME_RE.match(_TZ_SUFFIX_RE.sub("", tail).strip())
    if m is None:
        raise DateTimeError(f"invalid time value: {value!r}")
    hh, mm = int(m.group(1)), int(m.group(2))
    ss = int(m.group(3)) if m.group(3) else 0
    if ss == 60:
        # A leap second carries forward, as Postgres does: '23:59:60'::time is
        # '24:00:00' (the upper bound of the time domain), '10:00:60' is
        # '10:01:00'. Storing the literal 60 instead produced a value nothing
        # downstream could parse — Python's time rejects second 60 — so any
        # arithmetic on it died with a bare ValueError.
        ss = 0
        mm += 1
        hh, mm = hh + mm // 60, mm % 60
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
        # Same full-timestamp tolerance as ``parse_time`` — but here the zone
        # offset must survive, so retry the timetz matcher on the time part.
        tail = _time_part_of_timestamp(s)
        if tail is not None:
            m = _TIMETZ_RE.match(tail)
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
    seconds = int(rest[4:6]) if len(rest) > 4 else 0
    # A sub-minute zone offset keeps its seconds (historical LMT zones, e.g.
    # '00:00:00+01:01:03'::timetz); a whole-minute offset stays HH:MM.
    if seconds:
        return f"{sign}{hours:02d}:{minutes:02d}:{seconds:02d}"
    return f"{sign}{hours:02d}:{minutes:02d}"


def render_timetz(value: Any) -> str:
    """A stored timetz (canonical ``HH:MM:SS[.f]+HH:MM[:SS]``) in Postgres'
    output spelling: trailing all-zero offset groups are dropped, so ``+01:00``
    renders ``+01`` (pgjdbc's TimezoneTest asserts ``15:00:00+01``) while
    ``+05:30`` and a sub-minute ``+01:01:03`` keep their groups."""
    text = str(value)
    idx = max(text.rfind("+"), text.rfind("-"))
    if idx <= 0:
        return text
    base, sign, groups = text[:idx], text[idx], text[idx + 1 :].split(":")
    while len(groups) > 1 and groups[-1] == "00":
        groups.pop()
    return base + sign + ":".join(groups)


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


def is_timetz_value(v: Any) -> bool:
    """Whether ``v`` is a stored ``timetz`` — a time of day carrying a zone
    offset, which ``is_time_value`` deliberately does not match."""
    return isinstance(v, str) and _TIMETZ_RE.match(v) is not None


def split_timetz(v: Any) -> tuple[str, str]:
    """A ``timetz`` split into its canonical (time-of-day, offset) halves."""
    text = parse_timetz(v)
    idx = max(text.rfind("+"), text.rfind("-"))
    return text[:idx], text[idx:]


def to_date_obj(v: Any) -> _dt.date:
    if isinstance(v, _dt.datetime):
        return v.date()
    if isinstance(v, _dt.date):
        return v
    return _dt.date.fromisoformat(str(v)[:10])


def to_time_obj(v: Any) -> _dt.time:
    return _dt.time.fromisoformat(parse_time(v))


MICROS_PER_DAY = 86_400_000_000


def time_micros(v: Any) -> int:
    """A ``time`` as microseconds since midnight.

    Time arithmetic goes through this rather than ``to_time_obj`` because
    Postgres' ``time`` domain runs to ``24:00:00`` inclusive and Python's
    ``datetime.time`` stops one microsecond short — so the boundary value a
    leap second rolls into cannot be held in a ``time`` at all."""
    text = parse_time(v)
    hh, mm, rest = text.split(":", 2)
    ss, _, frac = rest.partition(".")
    micros = int((frac + "000000")[:6]) if frac else 0
    return ((int(hh) * 3600 + int(mm) * 60 + int(ss)) * 1_000_000) + micros


def time_from_micros(micros: int) -> str:
    """The canonical ``time`` text for a microsecond offset within one day."""
    return _fmt_time(
        micros // 3_600_000_000,
        micros // 60_000_000 % 60,
        micros // 1_000_000 % 60,
        f"{micros % 1_000_000:06d}" if micros % 1_000_000 else "",
    )


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

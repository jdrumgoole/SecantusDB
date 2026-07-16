"""Postgres ``interval`` type.

A Postgres interval has three independent components — ``months``, ``days``, and
``micros`` (microseconds) — kept separate because a month is not a fixed number
of days and (in real Postgres) a day is not always 24 hours. The canonical value
is a subdocument ``{"interval": {"months": m, "days": d, "micros": u}}`` so it
round-trips through BSON storage; ``secantus.sql`` ``scalar`` / ``typemap`` /
``planner`` wire it into the SQL surface.

Out of scope: DST-aware day arithmetic (days are treated as 24h), the ``@`` /
``ago`` verbose input grammar beyond a trailing ``ago``, and interval indexes.
"""

from __future__ import annotations

import calendar
import datetime as _dt
import re
from typing import Any

MICROS_PER_SECOND = 1_000_000
MICROS_PER_DAY = 86_400 * MICROS_PER_SECOND


class IntervalError(ValueError):
    """A malformed interval literal."""


# ``1d`` / ``5.5min`` — a number with its unit attached in one token.
_ATTACHED_RE = re.compile(r"([+-]?\d+(?:\.\d+)?)([A-Za-z]+)\Z")


# Unit name (singular) -> ("months" | "days" | "micros", multiplier). Postgres
# accepts abbreviated unit spellings (``1 sec``, ``5 min``, ``2 hr``, ``1d``) —
# the singularised abbreviations live here alongside the full names.
_UNIT_FIELD: dict[str, tuple[str, int]] = {
    "microsecond": ("micros", 1),
    "us": ("micros", 1),
    "usec": ("micros", 1),
    "millisecond": ("micros", 1_000),
    "ms": ("micros", 1_000),
    "msec": ("micros", 1_000),
    "second": ("micros", MICROS_PER_SECOND),
    "s": ("micros", MICROS_PER_SECOND),
    "sec": ("micros", MICROS_PER_SECOND),
    "minute": ("micros", 60 * MICROS_PER_SECOND),
    "m": ("micros", 60 * MICROS_PER_SECOND),
    "min": ("micros", 60 * MICROS_PER_SECOND),
    "hour": ("micros", 3600 * MICROS_PER_SECOND),
    "h": ("micros", 3600 * MICROS_PER_SECOND),
    "hr": ("micros", 3600 * MICROS_PER_SECOND),
    "day": ("days", 1),
    "d": ("days", 1),
    "week": ("days", 7),
    "w": ("days", 7),
    "month": ("months", 1),
    "mon": ("months", 1),
    "quarter": ("months", 3),
    "year": ("months", 12),
    "y": ("months", 12),
    "yr": ("months", 12),
    "decade": ("months", 120),
    "dec": ("months", 120),
    "century": ("months", 1200),
    "cent": ("months", 1200),
    "c": ("months", 1200),
    "millennium": ("months", 12000),
    "millennia": ("months", 12000),
    "mil": ("months", 12000),
}


def make(months: int = 0, days: int = 0, micros: int = 0) -> dict:
    return {"interval": {"months": int(months), "days": int(days), "micros": int(micros)}}


def _fields(subdoc: Any) -> tuple[int, int, int]:
    iv = subdoc["interval"] if isinstance(subdoc, dict) and "interval" in subdoc else subdoc
    return int(iv.get("months", 0)), int(iv.get("days", 0)), int(iv.get("micros", 0))


def is_interval(v: Any) -> bool:
    return isinstance(v, dict) and "interval" in v and isinstance(v["interval"], dict)


def _singular(unit: str) -> str:
    # Exact spellings first — stripping the plural 's' blindly would turn the
    # abbreviations ``ms`` / ``us`` into minutes / an unknown unit.
    u = unit.lower().strip()
    if u in _UNIT_FIELD:
        return u
    if u.endswith("s") and u[:-1] in _UNIT_FIELD:
        return u[:-1]
    return u


def from_unit(value: float, unit: str) -> dict:
    """A single ``<value> <unit>`` term -> an interval subdocument. A fractional
    months value overflows into days (30-day months) and micros, matching Postgres."""
    u = _singular(unit)
    if u not in _UNIT_FIELD:
        raise IntervalError(f"unsupported interval unit: {unit}")
    field, mult = _UNIT_FIELD[u]
    months = days = micros = 0
    scaled = value * mult
    if field == "months":
        months = int(scaled)
        frac_months = scaled - months
        # Postgres spills a fractional month into 30-day days, then into time.
        day_val = frac_months * 30
        days = int(day_val)
        micros = round((day_val - days) * MICROS_PER_DAY)
    elif field == "days":
        days = int(scaled)
        micros = round((scaled - days) * MICROS_PER_DAY)
    else:
        micros = round(scaled)
    return make(months, days, micros)


_TIME_RE = re.compile(r"^([+-]?)(\d+):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?$")


def _parse_time(token: str) -> int | None:
    """Parse an ``HH:MM[:SS[.ffffff]]`` clock token to signed micros, or None."""
    m = _TIME_RE.match(token)
    if m is None:
        return None
    sign = -1 if m.group(1) == "-" else 1
    hh = int(m.group(2))
    mm = int(m.group(3))
    ss = int(m.group(4)) if m.group(4) else 0
    frac = m.group(5) or ""
    micros = int((frac + "000000")[:6]) if frac else 0
    total = ((hh * 3600 + mm * 60 + ss) * MICROS_PER_SECOND) + micros
    return sign * total


def parse(text: str) -> dict:
    """Parse an interval literal — the Postgres output form (``1 year 2 mons 3 days
    04:05:06``), a verbose form (``1 year 2 months``), a bare unit term (``90
    minutes``), or a bare clock (``04:05:06``). A trailing ``ago`` negates."""
    s = str(text).strip()
    if not s:
        return make()
    negate = False
    if s.lower().endswith(" ago"):
        negate = True
        s = s[:-4].strip()
    months = days = micros = 0
    tokens = s.split()
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        clock = _parse_time(tok)
        if clock is not None:
            micros += clock
            i += 1
            continue
        try:
            value = float(tok)
        except ValueError:
            # Attached-unit form (``1d`` / ``3h`` / ``5.5min``) — number and
            # unit in one token.
            m_att = _ATTACHED_RE.match(tok)
            if m_att is None:
                raise IntervalError(f"invalid interval literal: {text!r}") from None
            part = from_unit(float(m_att.group(1)), m_att.group(2))
            pm, pd, pu = _fields(part)
            months += pm
            days += pd
            micros += pu
            i += 1
            continue
        if i + 1 >= len(tokens):
            # A bare trailing number is seconds (``'0'`` / ``'30'``), matching
            # Postgres' lenient interval input.
            micros += round(value * MICROS_PER_SECOND)
            i += 1
            continue
        part = from_unit(value, tokens[i + 1])
        pm, pd, pu = _fields(part)
        months += pm
        days += pd
        micros += pu
        i += 2
    iv = make(months, days, micros)
    return neg(iv) if negate else iv


def render(subdoc: Any) -> str:
    """Render an interval in the Postgres ``postgres`` output style, e.g.
    ``1 year 2 mons 3 days 04:05:06`` (each component carries its own sign)."""
    months, days, micros = _fields(subdoc)
    parts: list[str] = []
    sign_m = -1 if months < 0 else 1
    years = sign_m * (abs(months) // 12)
    mons = sign_m * (abs(months) % 12)
    if years:
        parts.append(f"{years} year{'s' if abs(years) != 1 else ''}")
    if mons:
        parts.append(f"{mons} mon{'s' if abs(mons) != 1 else ''}")
    if days:
        parts.append(f"{days} day{'s' if abs(days) != 1 else ''}")
    if micros != 0 or not parts:
        neg_t = micros < 0
        m = abs(micros)
        secs_total, frac = divmod(m, MICROS_PER_SECOND)
        hh, rem = divmod(secs_total, 3600)
        mm, ss = divmod(rem, 60)
        t = f"{hh:02d}:{mm:02d}:{ss:02d}"
        if frac:
            t += f".{frac:06d}".rstrip("0")
        parts.append(("-" if neg_t else "") + t)
    return " ".join(parts)


def add(a: Any, b: Any) -> dict:
    am, ad, au = _fields(a)
    bm, bd, bu = _fields(b)
    return make(am + bm, ad + bd, au + bu)


def sub(a: Any, b: Any) -> dict:
    am, ad, au = _fields(a)
    bm, bd, bu = _fields(b)
    return make(am - bm, ad - bd, au - bu)


def neg(a: Any) -> dict:
    m, d, u = _fields(a)
    return make(-m, -d, -u)


def mul(a: Any, factor: float) -> dict:
    m, d, u = _fields(a)
    # A fractional product spills months -> days -> micros the way Postgres does.
    months = m * factor
    whole_months = int(months)
    day_spill = (months - whole_months) * 30
    days = d * factor + day_spill
    whole_days = int(days)
    micro_spill = (days - whole_days) * MICROS_PER_DAY
    micros = round(u * factor + micro_spill)
    return make(whole_months, whole_days, micros)


def total_micros(a: Any) -> int:
    """The interval's justified duration in microseconds (1 month = 30 days,
    1 day = 24 h) — the value Postgres compares and sorts intervals by."""
    months, days, micros = _fields(a)
    return (months * 30 + days) * MICROS_PER_DAY + micros


def justify_days(a: Any) -> dict:
    """Roll every 30 days up into a month."""
    m, d, u = _fields(a)
    extra, d = divmod(d, 30) if d >= 0 else _divmod_toward_zero(d, 30)
    return make(m + extra, d, u)


def justify_hours(a: Any) -> dict:
    """Roll every 24 hours up into a day."""
    m, d, u = _fields(a)
    extra, u = divmod(u, MICROS_PER_DAY) if u >= 0 else _divmod_toward_zero(u, MICROS_PER_DAY)
    return make(m, d + extra, u)


def justify_interval(a: Any) -> dict:
    return justify_days(justify_hours(a))


def _divmod_toward_zero(n: int, d: int) -> tuple[int, int]:
    """divmod for a negative ``n`` that keeps the remainder's sign with ``n`` (so
    ``justify_hours`` rolls -25h into -1 day -1h, not -2 days +23h)."""
    q = -((-n) // d)
    r = n - q * d
    return q, r


def to_date(base: Any, subdoc: Any, sign: int) -> Any:
    """Apply ``sign * interval`` to a ``date`` / ``datetime``. A result outside
    Python's datetime range (PG's is far wider) is computed with proleptic
    ordinal math and returned as timestamp text."""
    months, days, micros = _fields(subdoc)
    try:
        result = _add_months(base, sign * months) if months else base
        return result + _dt.timedelta(days=sign * days, microseconds=sign * micros)
    except (OverflowError, ValueError):
        from secantus.sql import datetimes as _datetimes

        clock = (
            (base.hour * 3600 + base.minute * 60 + base.second) * 1_000_000 + base.microsecond
            if isinstance(base, _dt.datetime)
            else 0
        )
        total = clock + sign * micros
        day_shift, clock = divmod(total, MICROS_PER_DAY)
        # Months first (calendar-aware via ordinal month walk), then days.
        y, mo, d = base.year, base.month, base.day
        if months:
            t = y * 12 + (mo - 1) + sign * months
            y, mo = divmod(t, 12)
            mo += 1
            d = min(d, calendar.monthrange(2000 + (y % 4), mo)[1] if 1 <= y <= 9999 else 28)
        n = _datetimes.gregorian_ordinal(y, mo, d) + sign * days + day_shift
        y, mo, d = _datetimes.ordinal_to_gregorian(n)
        secs, us = divmod(clock, 1_000_000)
        hh, rem = divmod(secs, 3600)
        mi, ss = divmod(rem, 60)
        text = f"{max(y, 1 - y):04d}-{mo:02d}-{d:02d} {hh:02d}:{mi:02d}:{ss:02d}"
        if us:
            text += f".{us:06d}".rstrip("0")
        if y <= 0:
            text += " BC"
        return text


def _add_months(base: Any, months: int) -> Any:
    total = base.year * 12 + (base.month - 1) + months
    year, month = divmod(total, 12)
    month += 1
    day = min(base.day, calendar.monthrange(year, month)[1])
    return base.replace(year=year, month=month, day=day)


def diff(end: Any, start: Any) -> dict:
    """``timestamp - timestamp`` -> an interval expressed in days + micros (the way
    Postgres does — no month normalisation)."""
    end_dt, start_dt = _as_dt(end), _as_dt(start)
    delta = end_dt - start_dt
    micros = delta.days * MICROS_PER_DAY + delta.seconds * MICROS_PER_SECOND + delta.microseconds
    days, micros = (
        divmod(micros, MICROS_PER_DAY)
        if micros >= 0
        else _divmod_toward_zero(micros, MICROS_PER_DAY)
    )
    return make(0, days, micros)


def age(end: Any, start: Any) -> dict:
    """``age(end, start)`` -> a symbolic year/month/day interval (calendar-aware,
    borrowing from higher fields the way Postgres does)."""
    e, s = _as_dt(end), _as_dt(start)
    years = e.year - s.year
    months = e.month - s.month
    days = e.day - s.day
    micros = (
        (e.hour - s.hour) * 3600 + (e.minute - s.minute) * 60 + (e.second - s.second)
    ) * MICROS_PER_SECOND + (e.microsecond - s.microsecond)
    if micros < 0:
        micros += MICROS_PER_DAY
        days -= 1
    if days < 0:
        # Borrow the length of the month preceding `end`.
        prev_month = e.month - 1 or 12
        prev_year = e.year if e.month > 1 else e.year - 1
        days += calendar.monthrange(prev_year, prev_month)[1]
        months -= 1
    if months < 0:
        months += 12
        years -= 1
    return make(years * 12 + months, days, micros)


def extract_field(field: str, subdoc: Any) -> float:
    """``extract(<field> from interval)`` — the numeric field value."""
    months, days, micros = _fields(subdoc)
    f = field.lower().strip()
    secs_total = micros / MICROS_PER_SECOND
    if f in ("year", "years"):
        return months // 12
    if f in ("month", "months"):
        return months % 12
    if f in ("day", "days"):
        return days
    if f in ("hour", "hours"):
        return int(secs_total // 3600)
    if f in ("minute", "minutes"):
        return int((secs_total % 3600) // 60)
    if f in ("second", "seconds"):
        return (micros % (60 * MICROS_PER_SECOND)) / MICROS_PER_SECOND
    if f == "epoch":
        return months * 30 * 86400 + days * 86400 + micros / MICROS_PER_SECOND
    raise IntervalError(f"unsupported interval field: {field}")


def _as_dt(v: Any) -> _dt.datetime:
    if isinstance(v, _dt.datetime):
        return v
    if isinstance(v, _dt.date):
        return _dt.datetime(v.year, v.month, v.day)
    from secantus.sql.datetimes import parse_iso_datetime

    return parse_iso_datetime(v)

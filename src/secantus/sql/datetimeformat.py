"""Postgres ``to_char(<datetime>, fmt)`` template rendering.

The datetime half of ``to_char`` used to be built by round-tripping the
template through sqlglot's Postgres ``TIME_MAPPING`` and handing the result to
``strftime``. That mapping knows only a handful of tokens and matches single
letters anywhere they appear, which produced two whole classes of wrong answer:

* tokens it does not know were emitted **literally** — ``Q``, ``W``, ``CC``,
  ``J``, ``MS``, ``FF1``–``FF6``, ``RM``, ``Y,YYY``, ``TZH`` and a dozen more
  came out as their own spelling;
* tokens it does know were matched **inside other tokens** — the ``D`` in
  ``AD`` rendered the weekday, so ``to_char(ts, 'AD')`` answered ``'A3'`` and
  ``'A.D.'`` answered ``'A.3.'``.

So this module parses the Postgres template directly, longest token first, the
way ``formatting.c`` does. Everything here was measured against PostgreSQL
14.13 — see ``tests/test_sql_to_char_datetime.py``, whose expectations come
from the reference server rather than from this implementation.
"""

from __future__ import annotations

import datetime as _dt
import re
from typing import Any

#: Month in Roman numerals. Postgres blank-pads these to 4 characters on the
#: RIGHT (``'I   '``, ``'XII '``), which is the opposite of what right-aligned
#: numerals would suggest — measured, not assumed.
_ROMAN_MONTHS = [
    "I",
    "II",
    "III",
    "IV",
    "V",
    "VI",
    "VII",
    "VIII",
    "IX",
    "X",
    "XI",
    "XII",
]

#: ``Day`` and ``Month`` (the full names) are blank-padded to 9 characters.
#: ``FM`` and ``TM`` both suppress that.
_NAME_PAD = 9


def _ordinal_suffix(n: int) -> str:
    """``st`` / ``nd`` / ``rd`` / ``th`` for the ``TH`` suffix token."""
    n = abs(n)
    if n % 100 in (11, 12, 13):
        return "th"
    return {1: "st", 2: "nd", 3: "rd"}.get(n % 10, "th")


def _julian_day(year: int, month: int, day: int) -> int:
    """Julian Day Number of a proleptic-Gregorian date (the ``J`` token)."""
    a = (14 - month) // 12
    y = year + 4800 - a
    m = month + 12 * a - 3
    return day + (153 * m + 2) // 5 + 365 * y + y // 4 - y // 100 + y // 400 - 32045


def _apply_case(text: str, token: str) -> str:
    """Case the rendered text the way the TOKEN was spelled.

    Postgres reads the first two characters of the keyword as typed: two
    upper-case letters mean UPPER, an upper then a lower mean Capitalized,
    anything else lower. ``MONTH`` / ``Month`` / ``month`` is the canonical
    case, and the same rule drives ``DAY``, ``AM``, ``BC``, ``RM`` and ``TH``.
    """
    letters = [c for c in token if c.isalpha()][:2]
    if len(letters) >= 2 and letters[0].isupper() and letters[1].isupper():
        return text.upper()
    if letters and letters[0].isupper():
        return text.capitalize() if len(text) > 1 else text.upper()
    return text.lower()


def _dotted(text: str, token: str) -> str:
    """``A.M.`` / ``B.C.`` render with the dots the token carries."""
    dotted = ".".join(text) + "."
    return _apply_case(dotted, token)


def _offset_parts(ts: _dt.datetime) -> tuple[str, int, int]:
    """``(sign, hours, minutes)`` of ``ts``'s UTC offset — ``('+', 0, 0)`` when
    it is naive, which is what Postgres reports for a plain ``timestamp``
    rendered under a UTC session."""
    offset = ts.utcoffset() if ts.tzinfo is not None else None
    total = int(offset.total_seconds()) if offset is not None else 0
    sign = "-" if total < 0 else "+"
    total = abs(total)
    return sign, total // 3600, (total % 3600) // 60


def _fields(ts: _dt.datetime, zone_known: bool) -> dict[str, Any]:
    iso_year, iso_week, iso_dow = ts.isocalendar()[:3]
    doy = ts.timetuple().tm_yday
    sign, off_h, off_m = _offset_parts(ts)
    return {
        "year": ts.year,
        "iso_year": iso_year,
        "iso_week": iso_week,
        "iso_dow": iso_dow,
        "doy": doy,
        # ``D`` is 1=Sunday..7=Saturday, NOT the ISO weekday — this was off by
        # one for every day of the week.
        "dow": (ts.isoweekday() % 7) + 1,
        "secs_past_midnight": ts.hour * 3600 + ts.minute * 60 + ts.second,
        "offset_sign": sign,
        "offset_h": off_h,
        "offset_m": off_m,
        "zone_known": zone_known,
    }


def _render_token(token: str, ts: _dt.datetime, f: dict[str, Any]) -> tuple[str, bool] | None:
    """The text a single token renders, and whether it is NUMERIC (which is
    what decides whether a following ``TH`` suffix applies). ``None`` for a
    token this module does not implement."""
    up = token.upper()

    # --- year -------------------------------------------------------------
    if up == "Y,YYY":
        return f"{f['year']:04d}"[:-3] + "," + f"{f['year']:04d}"[-3:], True
    if up in ("YYYY", "YYY", "YY", "Y"):
        width = len(up)
        return f"{abs(f['year']):04d}"[-width:], True
    if up in ("IYYY", "IYY", "IY", "I"):
        width = len(up)
        return f"{abs(f['iso_year']):04d}"[-width:], True
    if up == "CC":
        year = f["year"]
        century = (year - 1) // 100 + 1 if year > 0 else (year // 100) - 1
        return f"{century:02d}", True
    if up in ("BC", "AD"):
        return _apply_case("AD" if f["year"] > 0 else "BC", token), False
    if up in ("B.C.", "A.D."):
        return _dotted("AD" if f["year"] > 0 else "BC", token), False

    # --- month / day names ------------------------------------------------
    if up == "MONTH":
        return _apply_case(ts.strftime("%B"), token).ljust(_NAME_PAD), False
    if up == "MON":
        return _apply_case(ts.strftime("%b"), token), False
    if up == "RM":
        return _apply_case(_ROMAN_MONTHS[ts.month - 1], token).ljust(4), False
    if up == "DAY":
        return _apply_case(ts.strftime("%A"), token).ljust(_NAME_PAD), False
    if up == "DY":
        return _apply_case(ts.strftime("%a"), token), False

    # --- date numbers -----------------------------------------------------
    if up == "MM":
        return f"{ts.month:02d}", True
    if up == "Q":
        return str((ts.month - 1) // 3 + 1), True
    if up == "IDDD":
        return f"{(f['iso_week'] - 1) * 7 + f['iso_dow']:03d}", True
    if up == "DDD":
        return f"{f['doy']:03d}", True
    if up == "DD":
        return f"{ts.day:02d}", True
    if up == "ID":
        return str(f["iso_dow"]), True
    if up == "D":
        return str(f["dow"]), True
    if up == "WW":
        return f"{(f['doy'] - 1) // 7 + 1:02d}", True
    if up == "IW":
        return f"{f['iso_week']:02d}", True
    if up == "W":
        return str((ts.day - 1) // 7 + 1), True
    if up == "J":
        return str(_julian_day(ts.year, ts.month, ts.day)), True

    # --- time -------------------------------------------------------------
    if up == "HH24":
        return f"{ts.hour:02d}", True
    if up in ("HH", "HH12"):
        return f"{(ts.hour + 11) % 12 + 1:02d}", True
    if up == "MI":
        return f"{ts.minute:02d}", True
    if up in ("SSSSS", "SSSS"):
        return str(f["secs_past_midnight"]), True
    if up == "SS":
        return f"{ts.second:02d}", True
    if up == "MS":
        return f"{ts.microsecond // 1000:03d}", True
    if up == "US":
        return f"{ts.microsecond:06d}", True
    if len(up) == 3 and up.startswith("FF") and up[2].isdigit() and up[2] != "0":
        return f"{ts.microsecond:06d}"[: int(up[2])], True
    if up in ("AM", "PM"):
        return _apply_case("PM" if ts.hour >= 12 else "AM", token), False
    if up in ("A.M.", "P.M."):
        return _dotted("PM" if ts.hour >= 12 else "AM", token), False

    # --- timezone ---------------------------------------------------------
    if up == "TZ":
        # A plain `timestamp` has no zone to name, so Postgres renders TZ as
        # empty even though OF/TZH/TZM still report the session offset. Only a
        # `timestamptz` names its zone.
        name = (ts.tzname() or "") if f["zone_known"] else ""
        return _apply_case(name, token) if name else "", False
    if up == "TZH":
        return f"{f['offset_sign']}{f['offset_h']:02d}", True
    if up == "TZM":
        return f"{f['offset_m']:02d}", True
    if up == "OF":
        text = f"{f['offset_sign']}{f['offset_h']:02d}"
        return (text + f":{f['offset_m']:02d}" if f["offset_m"] else text), False
    return None


#: Every token Postgres registers, as the exact SPELLINGS it registers them in.
#:
#: Matching is case-**sensitive**, which is not an implementation detail — it is
#: observable. ``formatting.c`` registers an all-upper and an all-lower variant
#: of every keyword, plus a Capitalized variant of the four word tokens, and
#: nothing else. So ``Ddth`` is not ``Dd`` + ``th``: it is ``D`` (4) then ``d``
#: (4 again) then ``th``, and Postgres answers ``'44th'``. Matching these
#: case-insensitively answered ``'02nd'``.
_WORD_TOKENS = ["MONTH", "MON", "DAY", "DY"]

#: Canonical (upper-case) spellings, longest first. ``MONTH`` must beat ``MON``
#: must beat ``MM``/``MI``/``MS``; ``IDDD`` must beat ``ID`` and ``I``;
#: ``SSSSS`` must beat ``SSSS`` must beat ``SS``.
_CANONICAL = [
    "Y,YYY",
    "A.M.",
    "P.M.",
    "A.D.",
    "B.C.",
    "IDDD",
    "IYYY",
    "SSSSS",
    "MONTH",
    "HH24",
    "HH12",
    "SSSS",
    "YYYY",
    "IYY",
    "DDD",
    "DAY",
    "FF1",
    "FF2",
    "FF3",
    "FF4",
    "FF5",
    "FF6",
    "YYY",
    "TZH",
    "TZM",
    "MON",
    "AM",
    "PM",
    "AD",
    "BC",
    "CC",
    "DD",
    "DY",
    "HH",
    "ID",
    "IW",
    "IY",
    "MI",
    "MM",
    "MS",
    "SS",
    "US",
    "WW",
    "YY",
    "TZ",
    "OF",
    "RM",
    "D",
    "I",
    "J",
    "Q",
    "W",
    "Y",
]


#: The three tokens Postgres registers ONLY in upper case. Measured: `of`
#: renders the literal 'of' (so `'DDth of Month'` is a date with the English
#: word in it, not an offset), and `tzh` renders `tz` + a literal 'h'.
_UPPER_ONLY = {"OF", "TZH", "TZM"}


def _spellings() -> list[str]:
    out: list[str] = []
    for token in _CANONICAL:
        out.append(token)
        if token not in _UPPER_ONLY:
            out.append(token.lower())
        if token in _WORD_TOKENS:
            out.append(token.capitalize())
    return sorted(set(out), key=len, reverse=True)


_TOKEN_RE = re.compile("|".join(re.escape(t) for t in _spellings()))
#: ``FM`` / ``TM`` / ``FX`` prefix a SINGLE token — ``FM`` is not a mode that
#: stays on. ``FMHH12:MI`` is ``'2:07'``, not ``'2:7'``.
_PREFIX_RE = re.compile(r"FM|fm|TM|tm|FX|fx")
_SUFFIX_RE = re.compile(r"TH|th|SP|sp")


def _emit(
    token: str, ts: _dt.datetime, f: dict[str, Any], *, fill: bool
) -> tuple[str, bool] | None:
    """Render one token, applying ``FM``/``TM`` fill mode if asked."""
    rendered = _render_token(token, ts, f)
    if rendered is None:
        return None
    text, numeric = rendered
    if fill:
        text = text.rstrip()
        if numeric:
            stripped = text.lstrip("0")
            text = stripped if stripped else "0"
    return text, numeric


def to_char_datetime(ts: _dt.datetime, fmt: str, *, zone_known: bool = True) -> str:
    """Render ``ts`` through the Postgres datetime template ``fmt``.

    ``zone_known`` is False for a plain ``timestamp``, which suppresses the
    ``TZ`` token's zone NAME while leaving the numeric offset tokens alone."""
    f = _fields(ts, zone_known)
    out: list[str] = []
    i, n = 0, len(fmt)
    last_numeric: int | None = None  # index in `out` of the last numeric token

    while i < n:
        ch = fmt[i]

        # Backslash escapes the next character.
        if ch == "\\" and i + 1 < n:
            out.append(fmt[i + 1])
            last_numeric = None
            i += 2
            continue

        # A double-quoted run is literal text, quotes stripped.
        if ch == '"':
            j, buf = i + 1, []
            while j < n and fmt[j] != '"':
                if fmt[j] == "\\" and j + 1 < n:
                    buf.append(fmt[j + 1])
                    j += 2
                    continue
                buf.append(fmt[j])
                j += 1
            out.append("".join(buf))
            last_numeric = None
            i = j + 1 if j < n else j
            continue

        # FM / TM prefix exactly one token; FX affects parsing only. An FM with
        # no token after it is dropped, as Postgres drops it.
        pre = _PREFIX_RE.match(fmt, i)
        if pre is not None:
            word = pre.group(0).upper()
            if word == "FX":
                i = pre.end()
                continue
            token = _TOKEN_RE.match(fmt, pre.end())
            if token is not None:
                emitted = _emit(token.group(0), ts, f, fill=True)
                if emitted is not None:
                    out.append(emitted[0])
                    last_numeric = len(out) - 1 if emitted[1] else None
                    i = token.end()
                    continue
            i = pre.end()
            continue

        # A TH / th suffix ordinalises the PRECEDING numeric token. With no
        # number before it, Postgres emits the suffix literally ('Th').
        suf = _SUFFIX_RE.match(fmt, i)
        if suf is not None and last_numeric is not None:
            word = suf.group(0)
            if word.upper() == "TH":
                value = out[last_numeric]
                suffix = _ordinal_suffix(int(value)) if value.lstrip("-").isdigit() else ""
                out[last_numeric] = value + _apply_case(suffix, word)
            last_numeric = None
            i = suf.end()
            continue

        match = _TOKEN_RE.match(fmt, i)
        if match is not None:
            emitted = _emit(match.group(0), ts, f, fill=False)
            if emitted is not None:
                out.append(emitted[0])
                last_numeric = len(out) - 1 if emitted[1] else None
                i = match.end()
                continue

        out.append(ch)
        last_numeric = None
        i += 1

    return "".join(out)


# --------------------------------------------------------------------------
# Parsing: to_date / to_timestamp
# --------------------------------------------------------------------------

#: Maximum digits a numeric token consumes when PARSING. Postgres reads up to
#: this many and stops early at a non-digit, so `to_date('2026-9-2',
#: 'YYYY-MM-DD')` works even though the month is one digit.
_PARSE_WIDTH = {
    "YYYY": 4,
    "YYY": 3,
    "YY": 2,
    "Y": 1,
    "IYYY": 4,
    "IYY": 3,
    "IY": 2,
    "I": 1,
    "MM": 2,
    "DD": 2,
    "DDD": 3,
    "IDDD": 3,
    "ID": 1,
    "D": 1,
    "HH": 2,
    "HH12": 2,
    "HH24": 2,
    "MI": 2,
    "SS": 2,
    "SSSS": 5,
    "SSSSS": 5,
    "MS": 3,
    "US": 6,
    "WW": 2,
    "IW": 2,
    "W": 1,
    "CC": 2,
    "Q": 1,
    "J": 7,
}

_MONTH_NAMES = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
]
_DAY_NAMES = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
]


def _date_from_julian(jdn: int) -> tuple[int, int, int]:
    """Inverse of ``_julian_day`` — the ``J`` token on the parsing side."""
    a = jdn + 32044
    b = (4 * a + 3) // 146097
    c = a - 146097 * b // 4
    d = (4 * c + 3) // 1461
    e = c - 1461 * d // 4
    m = (5 * e + 2) // 153
    day = e - (153 * m + 2) // 5 + 1
    month = m + 3 - 12 * (m // 10)
    year = 100 * b + d - 4800 + m // 10
    return year, month, day


class _ParseError(ValueError):
    """The input did not match the template."""


def _take_digits(text: str, pos: int, width: int) -> tuple[int, int]:
    """Read up to ``width`` digits (with an optional sign) starting at ``pos``."""
    start = pos
    if pos < len(text) and text[pos] in "+-":
        pos += 1
    while pos < len(text) and pos - start < width + (1 if text[start] in "+-" else 0):
        if not text[pos].isdigit():
            break
        pos += 1
    if pos == start or (pos == start + 1 and text[start] in "+-"):
        raise _ParseError(f"expected a number at offset {start}")
    return int(text[start:pos]), pos


def _take_name(text: str, pos: int, names: list[str], abbrev: bool) -> tuple[int, int]:
    """Match a month / day name (or its 3-letter abbreviation) case-insensitively.
    Returns its 1-based index."""
    lowered = text.lower()
    if abbrev:
        candidate = lowered[pos : pos + 3]
        for i, name in enumerate(names):
            if candidate == name[:3]:
                return i + 1, pos + 3
        raise _ParseError(f"expected a name at offset {pos}")
    best: tuple[int, int] | None = None
    for i, name in enumerate(names):
        if lowered.startswith(name, pos) and (best is None or len(name) > best[1] - pos):
            best = (i + 1, pos + len(name))
    if best is None:
        raise _ParseError(f"expected a name at offset {pos}")
    return best


def parse_datetime(text: str, fmt: str) -> _dt.datetime:
    """Parse ``text`` against the Postgres template ``fmt``.

    Shares the token table with `to_char_datetime`, which is the point: the
    parsing side used to convert the template to strftime through sqlglot's
    lossy mapping, so every word, meridiem, fractional-second and ISO-week
    template raised `22007 invalid input syntax` instead of parsing.
    """
    got: dict[str, int] = {}
    meridiem: str | None = None
    i, p = 0, 0
    n, tn = len(fmt), len(text)

    def skip_separators() -> None:
        nonlocal p
        while p < tn and not text[p].isalnum():
            p += 1

    while i < n:
        ch = fmt[i]
        if ch == '"':
            j = i + 1
            while j < n and fmt[j] != '"':
                j += 1
            i = j + 1 if j < n else j
            skip_separators()
            continue

        pre = _PREFIX_RE.match(fmt, i)
        if pre is not None:
            i = pre.end()
            continue

        match = _TOKEN_RE.match(fmt, i)
        if match is None:
            if not ch.isalnum():
                skip_separators()
            i += 1
            continue

        token = match.group(0)
        up = token.upper()
        i = match.end()
        skip_separators()
        if p >= tn and up not in ("AM", "PM", "A.M.", "P.M."):
            raise _ParseError("input ended early")

        if up == "MONTH":
            got["month"], p = _take_name(text, p, _MONTH_NAMES, abbrev=False)
        elif up == "MON":
            got["month"], p = _take_name(text, p, _MONTH_NAMES, abbrev=True)
        elif up == "DAY":
            _, p = _take_name(text, p, _DAY_NAMES, abbrev=False)
        elif up == "DY":
            _, p = _take_name(text, p, _DAY_NAMES, abbrev=True)
        elif up in ("AM", "PM", "A.M.", "P.M."):
            head = text[p : p + len(token)].upper().replace(".", "")
            if head[:1] in ("A", "P"):
                meridiem = head[:1]
                p += len(token) if "." in token else 2
        elif up in ("AD", "BC", "A.D.", "B.C."):
            p += len(token)
        elif up in ("TZ", "OF", "TZH", "TZM"):
            while p < tn and (text[p].isalnum() or text[p] in "+-:"):
                p += 1
        elif len(up) == 3 and up.startswith("FF") and up[2].isdigit():
            width = int(up[2])
            value, p = _take_digits(text, p, width)
            got["micro"] = value * 10 ** (6 - width)
        elif up in _PARSE_WIDTH:
            value, p = _take_digits(text, p, _PARSE_WIDTH[up])
            key = {
                "YYYY": "year",
                "YYY": "year",
                "YY": "year2",
                "Y": "year",
                "IYYY": "iso_year",
                "IYY": "iso_year",
                "IY": "iso_year",
                "I": "iso_year",
                "MM": "month",
                "DD": "day",
                "DDD": "doy",
                "IDDD": "iso_doy",
                "ID": "iso_dow",
                "D": "dow",
                "HH": "hour12",
                "HH12": "hour12",
                "HH24": "hour",
                "MI": "minute",
                "SS": "second",
                "SSSS": "secs_past_midnight",
                "SSSSS": "secs_past_midnight",
                "MS": "milli",
                "US": "micro",
                "WW": "week",
                "IW": "iso_week",
                "W": "week_of_month",
                "CC": "century",
                "Q": "quarter",
                "J": "julian",
            }[up]
            got[key] = value
        else:  # RM and anything else with no parsing meaning
            while p < tn and text[p].isalpha():
                p += 1

    # --- assemble ---------------------------------------------------------
    if "year2" in got:
        # Postgres reads a 2-digit year as 2000-2069 / 1970-1999.
        two = got["year2"]
        got["year"] = 2000 + two if two < 70 else 1900 + two
    if "milli" in got:
        got["micro"] = got["milli"] * 1000

    if "julian" in got:
        year, month, day = _date_from_julian(got["julian"])
    elif "iso_year" in got:
        iso_dow = got.get("iso_dow", 1)
        if "iso_doy" in got:
            week, iso_dow = (got["iso_doy"] - 1) // 7 + 1, (got["iso_doy"] - 1) % 7 + 1
        else:
            week = got.get("iso_week", 1)
        date = _dt.date.fromisocalendar(got["iso_year"], week, iso_dow)
        year, month, day = date.year, date.month, date.day
    elif "doy" in got:
        date = _dt.date(got.get("year", 1), 1, 1) + _dt.timedelta(days=got["doy"] - 1)
        year, month, day = date.year, date.month, date.day
    else:
        year, month, day = got.get("year", 1), got.get("month", 1), got.get("day", 1)

    hour = got.get("hour", 0)
    if "hour12" in got:
        hour = got["hour12"] % 12
        if meridiem == "P":
            hour += 12
    minute, second = got.get("minute", 0), got.get("second", 0)
    if "secs_past_midnight" in got:
        total = got["secs_past_midnight"]
        hour, minute, second = total // 3600, (total % 3600) // 60, total % 60
    return _dt.datetime(year, month, day, hour, minute, second, got.get("micro", 0))

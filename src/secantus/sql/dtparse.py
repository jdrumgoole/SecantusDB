"""PostgreSQL's general date/time INPUT parser, ported to Python.

A transcription of ``ParseDateTime`` and ``DecodeDateTime`` from PostgreSQL's
``src/backend/utils/adt/datetime.c`` (15). It reads everything a client may
send as a ``date`` / ``timestamp`` / ``timestamptz`` literal: ISO shapes,
``Jan 5, 2020``, ``5 January 2020``, ``1/5/2020`` in the session's DateStyle
order, ``20200105``, ``2020.005``, ``J2458854``, ``y2020m01d05``,
``January 5 2020 10:30 PM EST``, ``2020-01-05T10:30:00Z``, full IANA zone
names, and the reserved words (``epoch``, ``today``, ``allballs`` ...).

The field masks and the order in which fields are tried are PostgreSQL's,
because that order IS the semantics: whether ``02-03-04`` is a year, a month
or a day depends on what was seen before it. The structure follows the Rust
server's transcription (``crates/secantus-pgplan/src/dtparse.rs``), but every
behaviour here was checked against PostgreSQL itself.
"""

from __future__ import annotations

import datetime as _dt
import functools
from dataclasses import dataclass
from typing import Any

from secantus.sql import errors as _errors
from secantus.sql.datetimes import (
    DateTimeError,
    gregorian_ordinal,
    ordinal_to_gregorian,
)

# Field kinds from ParseDateTime.
_NUMBER, _STRING, _DATE, _TIME, _TZ, _SPECIAL = range(6)

# DTK_M bits.
YEAR = 1 << 2
MONTH = 1 << 1
DAY = 1 << 3
HOUR = 1 << 10
MINUTE = 1 << 11
SECOND = 1 << 12
MILLISECOND = 1 << 13
MICROSECOND = 1 << 14
TZ = 1 << 5
DOY = 1 << 15
DTZMOD = 1 << 6
DOW = 1 << 16
AMPM = 1 << 17
ADBC = 1 << 18
DATE_M = YEAR | MONTH | DAY
TIME_M = HOUR | MINUTE | SECOND | MILLISECOND | MICROSECOND
ALL_SECS_M = SECOND | MILLISECOND | MICROSECOND

# Units of the ISO labelled form (``y2020m01d05``) and ``J`` / ``T``.
U_YEAR, U_MONTH, U_DAY, U_HOUR, U_MINUTE, U_SECOND, U_JULIAN, U_TIME = range(1, 9)

# Failure kinds, as DateTimeParseError distinguishes them.
_BAD_FORMAT = "bad"
_FIELD_OVERFLOW = "overflow"
_MD_FIELD_OVERFLOW = "md_overflow"
_TZ_OVERFLOW = "tz_overflow"


class _Fail(Exception):
    def __init__(self, kind: str) -> None:
        super().__init__(kind)
        self.kind = kind


class DtParseError(_errors.SQLError, DateTimeError):
    """A PostgreSQL date/time input error: a SQLError carrying the exact
    SQLSTATE and message, and a ValueError for callers that soft-catch."""


_MONTHS = {
    "jan": 1, "january": 1, "feb": 2, "february": 2, "mar": 3, "march": 3,
    "apr": 4, "april": 4, "may": 5, "jun": 6, "june": 6, "jul": 7, "july": 7,
    "aug": 8, "august": 8, "sep": 9, "sept": 9, "september": 9, "oct": 10,
    "october": 10, "nov": 11, "november": 11, "dec": 12, "december": 12,
}  # fmt: skip
_DOWS = {
    "mon", "monday", "tue", "tues", "tuesday", "wed", "wednesday", "weds", "thu",
    "thur", "thurs", "thursday", "fri", "friday", "sat", "saturday", "sun", "sunday",
}  # fmt: skip
_UNITS = {
    "y": U_YEAR, "m": U_MONTH, "d": U_DAY, "h": U_HOUR, "mm": U_MINUTE,
    "s": U_SECOND, "j": U_JULIAN, "jd": U_JULIAN, "julian": U_JULIAN,
}  # fmt: skip
_RESERVED = {
    "epoch", "infinity", "+infinity", "-infinity", "now", "today", "tomorrow",
    "yesterday", "allballs",
}  # fmt: skip

#: PostgreSQL's ``Default`` timezone_abbreviations set (offset east in
#: seconds, is-daylight). The zone-dependent entries carry their current
#: offset.
_ABBREVS: dict[str, tuple[int, bool]] = {
    "acdt": (37800, True), "acsst": (37800, True), "acst": (34200, False),
    "act": (-18000, False), "acwst": (31500, False), "adt": (-10800, True),
    "aedt": (39600, True), "aesst": (39600, True), "aest": (36000, False),
    "aft": (16200, False), "akdt": (-28800, True), "akst": (-32400, False),
    "almst": (25200, True), "almt": (21600, False), "amst": (14400, False),
    "amt": (-14400, False), "anast": (43200, False), "anat": (43200, False),
    "arst": (-10800, False), "art": (-10800, False), "ast": (-14400, False),
    "awsst": (32400, True), "awst": (28800, False), "azost": (0, True),
    "azot": (-3600, False), "azst": (14400, False), "azt": (14400, False),
    "bdst": (7200, True), "bdt": (21600, False), "bnt": (28800, False),
    "bort": (28800, False), "bot": (-14400, False), "bra": (-10800, False),
    "brst": (-7200, True), "brt": (-10800, False), "bst": (3600, True),
    "btt": (21600, False), "cadt": (37800, True), "cast": (34200, False),
    "cct": (28800, False), "cdt": (-18000, True), "cest": (7200, True),
    "cet": (3600, False), "cetdst": (7200, True), "chadt": (49500, True),
    "chast": (45900, False), "chut": (36000, False), "ckt": (-36000, False),
    "clst": (-10800, True), "clt": (-10800, True), "cot": (-18000, False),
    "cst": (-21600, False), "cxt": (25200, False), "davt": (25200, False),
    "ddut": (36000, False), "easst": (-18000, True), "east": (-18000, True),
    "eat": (10800, False), "edt": (-14400, True), "eest": (10800, True),
    "eet": (7200, False), "eetdst": (10800, True), "egst": (0, True),
    "egt": (-3600, False), "est": (-18000, False), "fet": (10800, False),
    "fjst": (46800, True), "fjt": (43200, False), "fkst": (-10800, False),
    "fkt": (-10800, False), "fnst": (-3600, True), "fnt": (-7200, False),
    "galt": (-21600, False), "gamt": (-32400, False), "gest": (14400, False),
    "get": (14400, False), "gft": (-10800, False), "gilt": (43200, False),
    "gmt": (0, False), "gyt": (-14400, False), "hkt": (28800, False),
    "hst": (-36000, False), "ict": (25200, False), "idt": (10800, True),
    "iot": (21600, False), "irkst": (28800, False), "irkt": (28800, False),
    "irt": (12600, False), "ist": (7200, False), "jayt": (32400, False),
    "jst": (32400, False), "kdt": (36000, True), "kgst": (21600, True),
    "kgt": (21600, False), "kost": (39600, False), "krast": (25200, False),
    "krat": (25200, False), "kst": (32400, False), "lhdt": (37800, False),
    "lhst": (37800, False), "ligt": (36000, False), "lint": (50400, False),
    "lkt": (19800, False), "magst": (39600, False), "magt": (39600, False),
    "mart": (-34200, False), "mawt": (18000, False), "mdt": (-21600, True),
    "mest": (7200, True), "mesz": (7200, True), "met": (3600, False),
    "metdst": (7200, True), "mez": (3600, False), "mht": (43200, False),
    "mmt": (23400, False), "mpt": (36000, False), "msd": (14400, True),
    "msk": (10800, False), "mst": (-25200, False), "must": (18000, True),
    "mut": (14400, False), "mvt": (18000, False), "myt": (28800, False),
    "ndt": (-9000, True), "nft": (-12600, False), "novst": (25200, False),
    "novt": (25200, False), "npt": (20700, False), "nst": (-12600, False),
    "nut": (-39600, False), "nzdt": (46800, True), "nzst": (43200, False),
    "nzt": (43200, False), "omsst": (21600, False), "omst": (21600, False),
    "pdt": (-25200, True), "pet": (-18000, False), "petst": (43200, False),
    "pett": (43200, False), "pgt": (36000, False), "pht": (28800, False),
    "pkst": (21600, True), "pkt": (18000, False), "pmdt": (-7200, True),
    "pmst": (-10800, False), "pont": (39600, False), "pst": (-28800, False),
    "pwt": (32400, False), "pyst": (-10800, True), "pyt": (-10800, False),
    "ret": (14400, False), "sadt": (37800, True), "sast": (7200, False),
    "sct": (14400, False), "sgt": (28800, False), "taht": (-36000, False),
    "tft": (18000, False), "tjt": (18000, False), "tkt": (46800, False),
    "tmt": (18000, False), "tot": (46800, False), "trut": (36000, False),
    "tvt": (43200, False), "uct": (0, False), "ulast": (32400, True),
    "ulat": (28800, False), "ut": (0, False), "utc": (0, False),
    "uyst": (-7200, True), "uyt": (-10800, False), "uzst": (21600, True),
    "uzt": (18000, False), "vet": (-14400, False), "vlast": (36000, False),
    "vlat": (36000, False), "volt": (10800, False), "vut": (39600, False),
    "wadt": (28800, True), "wakt": (43200, False), "wast": (25200, False),
    "wat": (3600, False), "wdt": (32400, True), "wet": (0, False),
    "wetdst": (3600, True), "wft": (43200, False), "wgst": (-7200, True),
    "wgt": (-10800, False), "xjt": (21600, False), "yakst": (32400, False),
    "yakt": (32400, False), "yapt": (36000, False), "yekst": (21600, True),
    "yekt": (18000, False), "z": (0, False), "zulu": (0, False),
}  # fmt: skip


def _is_keyword(t: str) -> bool:
    return (
        t in _MONTHS
        or t in _DOWS
        or t in _UNITS
        or t in _RESERVED
        or t in _ABBREVS
        or t in ("am", "pm", "ad", "bc", "at", "on", "t", "dst")
    )


def abbreviation_offset(name: str) -> int | None:
    """A time zone abbreviation's offset in seconds east (``est`` is -18000)."""
    hit = _ABBREVS.get(name.lower())
    return hit[0] if hit else None


@functools.lru_cache(maxsize=1)
def _zone_names() -> dict[str, str]:
    import zoneinfo

    try:
        names = zoneinfo.available_timezones()
    except Exception:  # noqa: BLE001 — no tzdata: no full zone names
        names = set()
    return {n.lower(): n for n in names}


def resolve_zone(name: str) -> _dt.tzinfo | None:
    """A full IANA zone name, case-insensitively, as zoneinfo knows it."""
    import zoneinfo

    canonical = _zone_names().get(name.lower())
    if canonical is None:
        return None
    try:
        return zoneinfo.ZoneInfo(canonical)
    except Exception:  # noqa: BLE001
        return None


def _isdigit(c: str) -> bool:
    return "0" <= c <= "9"


def _isalpha(c: str) -> bool:
    return ("a" <= c <= "z") or ("A" <= c <= "Z")


def _isalnum(c: str) -> bool:
    return _isdigit(c) or _isalpha(c)


def _parse_fields(text: str) -> list[tuple[str, int]]:
    cs = text
    n = len(cs)
    i = 0
    out: list[tuple[str, int]] = []
    while i < n:
        c = cs[i]
        if c.isspace():
            i += 1
            continue
        f: list[str] = []
        if _isdigit(c):
            while i < n and _isdigit(cs[i]):
                f.append(cs[i])
                i += 1
            if i < n and cs[i] == ":":
                kind = _TIME
                while i < n and (_isdigit(cs[i]) or cs[i] in ":."):
                    f.append(cs[i])
                    i += 1
            elif i < n and cs[i] in "-/.":
                delim = cs[i]
                f.append(delim)
                i += 1
                if i < n and _isdigit(cs[i]):
                    k = _NUMBER if delim == "." else _DATE
                    while i < n and _isdigit(cs[i]):
                        f.append(cs[i])
                        i += 1
                    if i < n and cs[i] == delim:
                        k = _DATE
                        f.append(cs[i])
                        i += 1
                        while i < n and (_isdigit(cs[i]) or cs[i] == delim):
                            f.append(cs[i])
                            i += 1
                    kind = k
                else:
                    kind = _DATE
                    while i < n and (_isalnum(cs[i]) or cs[i] == delim):
                        f.append(cs[i].lower())
                        i += 1
            else:
                kind = _NUMBER
        elif c == ".":
            f.append(c)
            i += 1
            while i < n and _isdigit(cs[i]):
                f.append(cs[i])
                i += 1
            kind = _NUMBER
        elif _isalpha(c):
            kind = _STRING
            while i < n and _isalpha(cs[i]):
                f.append(cs[i].lower())
                i += 1
            word = "".join(f)
            if i < n and cs[i] in "-/.":
                is_date = True
            elif i < n and (cs[i] == "+" or _isdigit(cs[i])):
                is_date = not _is_keyword(word)
            else:
                is_date = False
            if is_date:
                kind = _DATE
                while True:
                    f.append(cs[i].lower())
                    i += 1
                    if not (i < n and (cs[i] in "+-/_.:" or _isalnum(cs[i]))):
                        break
        elif c in "+-":
            f.append(c)
            i += 1
            while i < n and cs[i].isspace():
                i += 1
            if i < n and _isdigit(cs[i]):
                kind = _TZ
                while i < n and (_isdigit(cs[i]) or cs[i] in ":.-"):
                    f.append(cs[i])
                    i += 1
            elif i < n and _isalpha(cs[i]):
                kind = _SPECIAL
                while i < n and _isalpha(cs[i]):
                    f.append(cs[i].lower())
                    i += 1
            else:
                raise _Fail(_BAD_FORMAT)
        elif c.isascii() and not c.isalnum() and c.isprintable():
            i += 1
            continue
        else:
            raise _Fail(_BAD_FORMAT)
        out.append(("".join(f), kind))
    return out


@dataclass
class _Tm:
    year: int = 0
    mon: int = 0
    mday: int = 0
    hour: int = 0
    min: int = 0
    sec: int = 0
    yday: int = 0
    fsec: int = 0


def _strtoint(s: str) -> tuple[int | None, str]:
    k = 0
    while k < len(s) and _isdigit(s[k]):
        k += 1
    if k == 0:
        return None, s
    return int(s[:k]), s[k:]


def _fraction(s: str) -> int:
    try:
        return round(float("0" + s) * 1_000_000)
    except ValueError:
        raise _Fail(_BAD_FORMAT) from None


def _decode_time(s: str, tm: _Tm) -> int:
    h, rest = _strtoint(s)
    if h is None or not rest.startswith(":"):
        raise _Fail(_BAD_FORMAT)
    m, rest = _strtoint(rest[1:])
    if m is None:
        raise _Fail(_BAD_FORMAT)
    tm.hour = h
    if rest == "":
        tm.min, tm.sec, tm.fsec = m, 0, 0
    elif rest.startswith("."):
        # mm:ss.sss
        tm.fsec = _fraction(rest)
        tm.sec = m
        tm.min = h
        tm.hour = 0
    elif rest.startswith(":"):
        tm.min = m
        sec, r = _strtoint(rest[1:])
        if sec is None:
            raise _Fail(_BAD_FORMAT)
        tm.sec = sec
        if r == "":
            tm.fsec = 0
        elif r.startswith("."):
            tm.fsec = _fraction(r)
        else:
            raise _Fail(_BAD_FORMAT)
    else:
        raise _Fail(_BAD_FORMAT)
    if tm.min > 59 or tm.sec > 60 or tm.fsec > 1_000_000:
        raise _Fail(_FIELD_OVERFLOW)
    return TIME_M


def _decode_timezone(s: str) -> int:
    if len(s) < 2 or s[0] not in "+-":
        raise _Fail(_BAD_FORMAT)
    neg = s[0] == "-"
    body = s[1:]
    hr, rest = _strtoint(body)
    if hr is None:
        raise _Fail(_BAD_FORMAT)
    mn = sec = 0
    if rest.startswith(":"):
        v, rest = _strtoint(rest[1:])
        if v is None:
            raise _Fail(_BAD_FORMAT)
        mn = v
        if rest.startswith(":"):
            v, rest = _strtoint(rest[1:])
            if v is None:
                raise _Fail(_BAD_FORMAT)
            sec = v
    elif rest == "" and len(body) > 2:
        mn = hr % 100
        hr //= 100
    if not (0 <= hr <= 15) or not (0 <= mn < 60) or not (0 <= sec < 60):
        raise _Fail(_TZ_OVERFLOW)
    if rest:
        raise _Fail(_BAD_FORMAT)
    tz = (hr * 60 + mn) * 60 + sec
    return -tz if neg else tz


class _State:
    def __init__(self, order: str) -> None:
        self.tm = _Tm()
        self.is2digits = False
        self.order = order


def _decode_number_field(s: str, fmask: int, st: _State) -> int:
    tm = st.tm
    body = s
    dot = s.find(".")
    if dot >= 0:
        tm.fsec = _fraction(s[dot:])
        body = s[:dot]
    elif fmask & DATE_M != DATE_M and len(body) >= 6:
        ln = len(body)
        if not body.isdigit():
            raise _Fail(_BAD_FORMAT)
        tm.mday = int(body[ln - 2 :])
        tm.mon = int(body[ln - 4 : ln - 2])
        tm.year = int(body[: ln - 4])
        if ln - 4 == 2:
            st.is2digits = True
        return DATE_M
    ln = len(body)
    if fmask & TIME_M != TIME_M and body.isdigit():
        if ln == 6:
            tm.sec = int(body[4:6])
            tm.min = int(body[2:4])
            tm.hour = int(body[:2])
            return TIME_M
        if ln == 4:
            tm.sec = 0
            tm.min = int(body[2:4])
            tm.hour = int(body[:2])
            return TIME_M
    raise _Fail(_BAD_FORMAT)


def _decode_number(s: str, have_text_month: bool, fmask: int, st: _State) -> int:
    tm = st.tm
    flen = len(s)
    val, rest = _strtoint(s)
    if val is None:
        raise _Fail(_BAD_FORMAT)
    if val > 2**31 - 1:
        raise _Fail(_FIELD_OVERFLOW)
    if rest.startswith("."):
        if len(s) - len(rest) > 2:
            return _decode_number_field(s, fmask | DATE_M, st)
        tm.fsec = _fraction(rest)
    elif rest:
        raise _Fail(_BAD_FORMAT)
    if flen == 3 and fmask & DATE_M == YEAR and 1 <= val <= 366:
        tm.yday = val
        return DOY | MONTH | DAY
    order = st.order
    dm = fmask & DATE_M
    if dm == 0:
        if flen >= 3 or order == "YMD":
            tm.year = val
            tmask = YEAR
        elif order == "DMY":
            tm.mday = val
            tmask = DAY
        else:
            tm.mon = val
            tmask = MONTH
    elif dm == YEAR:
        tm.mon = val
        tmask = MONTH
    elif dm == MONTH:
        if have_text_month:
            if flen >= 3 or order == "YMD":
                tm.year = val
                tmask = YEAR
            else:
                tm.mday = val
                tmask = DAY
        else:
            tm.mday = val
            tmask = DAY
    elif dm == YEAR | MONTH:
        if have_text_month and flen >= 3 and st.is2digits:
            tm.mday = tm.year
            tm.year = val
            st.is2digits = False
        else:
            tm.mday = val
        tmask = DAY
    elif dm == DAY:
        tm.mon = val
        tmask = MONTH
    elif dm == MONTH | DAY:
        tm.year = val
        tmask = YEAR
    elif dm == DATE_M:
        return _decode_number_field(s, fmask, st)
    else:
        raise _Fail(_BAD_FORMAT)
    if tmask == YEAR:
        st.is2digits = flen <= 2
    return tmask


def _decode_date(s: str, fmask_in: int, st: _State) -> int:
    fields: list[str] = []
    i, n = 0, len(s)
    while i < n:
        while i < n and not _isalnum(s[i]):
            i += 1
        if i >= n:
            raise _Fail(_BAD_FORMAT)
        start = i
        if _isdigit(s[i]):
            while i < n and _isdigit(s[i]):
                i += 1
        else:
            while i < n and _isalpha(s[i]):
                i += 1
        fields.append(s[start:i])
        if i < n:
            i += 1
    fmask = fmask_in
    tmask = 0
    have_text_month = False
    used = [False] * len(fields)
    for k, f in enumerate(fields):
        if f and _isalpha(f[0]):
            if f in ("at", "on"):
                used[k] = True
                continue
            if f in _MONTHS:
                if fmask & MONTH:
                    raise _Fail(_BAD_FORMAT)
                st.tm.mon = _MONTHS[f]
                have_text_month = True
                fmask |= MONTH
                tmask |= MONTH
                used[k] = True
            else:
                raise _Fail(_BAD_FORMAT)
    for k, f in enumerate(fields):
        if used[k]:
            continue
        if not f:
            raise _Fail(_BAD_FORMAT)
        dmask = _decode_number(f, have_text_month, fmask, st)
        if fmask & dmask:
            raise _Fail(_BAD_FORMAT)
        fmask |= dmask
        tmask |= dmask
    if (fmask & ~(DOY | TZ)) != DATE_M:
        raise _Fail(_BAD_FORMAT)
    return tmask


def _date2j(y: int, m: int, d: int) -> int:
    if m > 2:
        m += 1
        y += 4800
    else:
        m += 13
        y += 4799
    century = y // 100
    julian = y * 365 - 32167
    julian += y // 4 - century + century // 4
    julian += 7834 * m // 256 + d
    return julian


def _j2date(jd: int) -> tuple[int, int, int]:
    julian = jd + 32044
    quad = julian // 146097
    extra = (julian - quad * 146097) * 4 + 3
    julian += 60 + quad * 3 + extra // 146097
    quad = julian // 1461
    julian -= quad * 1461
    y = julian * 4 // 1461
    julian = ((julian + 305) % 365 if y != 0 else (julian + 306) % 366) + 123
    y += quad * 4
    year = y - 4800
    quad = julian * 2141 // 65536
    day = julian - 7834 * quad // 256
    month = (quad + 10) % 12 + 1
    return year, month, day


def _is_leap(y: int) -> bool:
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)


_DAYS = (31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)


def _validate_date(fmask: int, isjulian: bool, is2digits: bool, bc: bool, tm: _Tm) -> None:
    if fmask & YEAR:
        if isjulian:
            pass
        elif bc:
            if tm.year <= 0:
                raise _Fail(_FIELD_OVERFLOW)
            tm.year = -(tm.year - 1)
        elif is2digits:
            if tm.year < 70:
                tm.year += 2000
            elif tm.year < 100:
                tm.year += 1900
        elif tm.year <= 0:
            raise _Fail(_FIELD_OVERFLOW)
    if fmask & DOY:
        tm.year, tm.mon, tm.mday = _j2date(_date2j(tm.year, 1, 1) + tm.yday - 1)
    if fmask & MONTH and not 1 <= tm.mon <= 12:
        raise _Fail(_MD_FIELD_OVERFLOW)
    if fmask & DAY and not 1 <= tm.mday <= 31:
        raise _Fail(_MD_FIELD_OVERFLOW)
    if fmask & DATE_M == DATE_M:
        dim = 29 if tm.mon == 2 and _is_leap(tm.year) else _DAYS[tm.mon - 1]
        if tm.mday > dim:
            raise _Fail(_FIELD_OVERFLOW)


@dataclass
class Parsed:
    """A decoded date/time. ``year`` is astronomical (1 BC is 0)."""

    year: int = 0
    month: int = 0
    day: int = 0
    hour: int = 0
    minute: int = 0
    second: int = 0
    micros: int = 0
    #: Offset EAST of UTC in seconds, when the input named one.
    offset: int | None = None
    #: A full zone name (``america/new_york``) the input named.
    zone: _dt.tzinfo | None = None
    #: ``epoch`` / ``infinity`` / ``-infinity`` / ``now``.
    special: str | None = None
    #: ``today`` / ``yesterday`` / ``tomorrow``: days from the current date.
    relative_day: int | None = None
    has_time: bool = False


def _decode(text: str, order: str) -> Parsed:  # noqa: C901, PLR0912, PLR0915
    fields = _parse_fields(text)
    nf = len(fields)
    st = _State(order)
    tm = st.tm
    fmask = 0
    ptype = 0
    mer: bool | None = None
    have_text_month = False
    isjulian = False
    bc = False
    out = Parsed()
    tz: int | None = None
    for i in range(nf):
        f, kind = fields[i]
        if kind == _DATE:
            if ptype == U_JULIAN:
                val, rest = _strtoint(f)
                if val is None:
                    raise _Fail(_BAD_FORMAT)
                tm.year, tm.mon, tm.mday = _j2date(val)
                isjulian = True
                tz = _decode_timezone(rest)
                tmask = DATE_M | TIME_M | TZ
                ptype = 0
            elif ptype != 0 or fmask & (MONTH | DAY) == (MONTH | DAY):
                if _isdigit(f[0]) or ptype != 0:
                    if ptype != 0:
                        if ptype != U_TIME:
                            raise _Fail(_BAD_FORMAT)
                        ptype = 0
                    if fmask & TIME_M == TIME_M:
                        raise _Fail(_BAD_FORMAT)
                    dash = f.find("-")
                    if dash < 0:
                        raise _Fail(_BAD_FORMAT)
                    tz = _decode_timezone(f[dash:])
                    tmask = _decode_number_field(f[:dash], fmask, st) | TZ
                else:
                    zone = resolve_zone(f)
                    if zone is None:
                        raise _Fail(_BAD_FORMAT)
                    out.zone = zone
                    tmask = TZ
            else:
                tmask = _decode_date(f, fmask, st)
        elif kind == _TIME:
            if ptype != 0:
                if ptype != U_TIME:
                    raise _Fail(_BAD_FORMAT)
                ptype = 0
            tmask = _decode_time(f, tm)
            total = tm.hour * 3600 + tm.min * 60 + tm.sec
            if total > 86_400 or (total == 86_400 and tm.fsec > 0):
                raise _Fail(_FIELD_OVERFLOW)
        elif kind == _TZ:
            tz = _decode_timezone(f)
            tmask = TZ
        elif kind == _NUMBER:
            if ptype != 0:
                val, rest = _strtoint(f)
                if val is None:
                    raise _Fail(_BAD_FORMAT)
                if rest.startswith("."):
                    if ptype not in (U_JULIAN, U_TIME, U_SECOND):
                        raise _Fail(_BAD_FORMAT)
                elif rest:
                    raise _Fail(_BAD_FORMAT)
                if ptype == U_YEAR:
                    tm.year = val
                    tmask = YEAR
                elif ptype == U_MONTH:
                    if fmask & MONTH and fmask & HOUR:
                        tm.min = val
                        tmask = MINUTE
                    else:
                        tm.mon = val
                        tmask = MONTH
                elif ptype == U_DAY:
                    tm.mday = val
                    tmask = DAY
                elif ptype == U_HOUR:
                    tm.hour = val
                    tmask = HOUR
                elif ptype == U_MINUTE:
                    tm.min = val
                    tmask = MINUTE
                elif ptype == U_SECOND:
                    tm.sec = val
                    if rest.startswith("."):
                        tm.fsec = _fraction(rest)
                        tmask = ALL_SECS_M
                    else:
                        tmask = SECOND
                elif ptype == U_JULIAN:
                    tm.year, tm.mon, tm.mday = _j2date(val)
                    isjulian = True
                    tmask = DATE_M
                    if rest.startswith("."):
                        us = round(float("0" + rest) * 86_400_000_000)
                        tm.hour = us // 3_600_000_000
                        tm.min = (us // 60_000_000) % 60
                        tm.sec = (us // 1_000_000) % 60
                        tm.fsec = us % 1_000_000
                        tmask |= TIME_M
                elif ptype == U_TIME:
                    tmask = _decode_number_field(f, fmask | DATE_M, st)
                    if tmask != TIME_M:
                        raise _Fail(_BAD_FORMAT)
                else:
                    raise _Fail(_BAD_FORMAT)
                ptype = 0
            else:
                flen = len(f)
                dot = f.find(".")
                if dot >= 0 and fmask & DATE_M == 0:
                    tmask = _decode_date(f, fmask, st)
                elif dot > 2 or (flen >= 6 and (fmask & DATE_M == 0 or fmask & TIME_M == 0)):
                    tmask = _decode_number_field(f, fmask, st)
                else:
                    tmask = _decode_number(f, have_text_month, fmask, st)
        else:  # _STRING / _SPECIAL
            if f in ("at", "on"):
                continue
            if f in _DOWS:
                tmask = DOW
            elif f in _RESERVED:
                tmask = DATE_M | TIME_M | TZ
                if f == "epoch":
                    out.special = "epoch"
                elif f in ("infinity", "+infinity"):
                    out.special = "infinity"
                elif f == "-infinity":
                    out.special = "-infinity"
                elif f == "now":
                    out.special = "now"
                elif f in ("today", "tomorrow", "yesterday"):
                    out.relative_day = {"yesterday": -1, "tomorrow": 1}.get(f, 0)
                    if fmask & DATE_M:
                        raise _Fail(_BAD_FORMAT)
                    fmask |= DATE_M
                    continue
                else:  # allballs
                    tm.hour = tm.min = tm.sec = 0
                    tz = 0
                    if fmask & (TIME_M | TZ):
                        raise _Fail(_BAD_FORMAT)
                    fmask |= TIME_M | TZ
                    continue
            elif f in _MONTHS:
                tmask = MONTH
                if fmask & MONTH and not have_text_month and not fmask & DAY and 1 <= tm.mon <= 31:
                    tm.mday = tm.mon
                    tmask = DAY
                have_text_month = True
                tm.mon = _MONTHS[f]
            elif f == "dst":
                tmask = DTZMOD
                tz = (tz or 0) + 3600
            elif f in _ABBREVS:
                tmask = TZ
                tz = _ABBREVS[f][0]
            elif f in ("am", "pm"):
                tmask = AMPM
                mer = f == "pm"
            elif f in ("ad", "bc"):
                tmask = ADBC
                bc = f == "bc"
            elif f in _UNITS:
                ptype = _UNITS[f]
                continue
            elif f == "t":
                if fmask & DATE_M != DATE_M:
                    raise _Fail(_BAD_FORMAT)
                if i + 1 >= nf or fields[i + 1][1] not in (_NUMBER, _TIME, _DATE):
                    raise _Fail(_BAD_FORMAT)
                ptype = U_TIME
                continue
            else:
                zone = resolve_zone(f) if kind == _STRING else None
                if zone is None:
                    raise _Fail(_BAD_FORMAT)
                out.zone = zone
                tmask = TZ
        if tmask & fmask:
            raise _Fail(_BAD_FORMAT)
        fmask |= tmask
    if ptype != 0:
        raise _Fail(_BAD_FORMAT)
    if out.special is not None:
        if nf != 1:
            raise _Fail(_BAD_FORMAT)
        return out
    if out.relative_day is None:
        _validate_date(fmask, isjulian, st.is2digits, bc, tm)
    if mer is not None:
        if tm.hour > 12:
            raise _Fail(_FIELD_OVERFLOW)
        if not mer and tm.hour == 12:
            tm.hour = 0
        elif mer and tm.hour != 12:
            tm.hour += 12
    if out.relative_day is None and fmask & DATE_M != DATE_M:
        raise _Fail(_BAD_FORMAT)
    out.year, out.month, out.day = tm.year, tm.mon, tm.mday
    out.hour, out.minute, out.second, out.micros = tm.hour, tm.min, tm.sec, tm.fsec
    out.offset = tz
    out.has_time = bool(fmask & TIME_M)
    return out


# ---------------------------------------------------------------------------
# Public entry points
# ---------------------------------------------------------------------------

_TYPE_NAMES = {
    "date": "date",
    "timestamp": "timestamp",
    "timestamptz": "timestamp with time zone",
}


def _error(kind: str, text: str, tag: str) -> DtParseError:
    if kind == _BAD_FORMAT:
        name = _TYPE_NAMES.get(tag, tag)
        return DtParseError("22007", f'invalid input syntax for type {name}: "{text}"')
    if kind == _TZ_OVERFLOW:
        return DtParseError("22009", f'time zone displacement out of range: "{text}"')
    diag = (
        {"H": 'Perhaps you need a different "datestyle" setting.'}
        if kind == _MD_FIELD_OVERFLOW
        else None
    )
    return DtParseError("22008", f'date/time field value out of range: "{text}"', diag=diag)


def _session() -> Any:
    from secantus.sql.typemap import _render_session

    return _render_session.get()


def session_date_order(session: Any = None) -> str:
    """``MDY`` / ``DMY`` / ``YMD`` from the session's DateStyle (MDY when
    unbound, PostgreSQL's default)."""
    if session is None:
        session = _session()
    if session is None:
        return "MDY"
    try:
        ds = session.get_setting("DateStyle") or ""
    except Exception:  # noqa: BLE001
        return "MDY"
    up = ds.upper()
    if "GERMAN" in up or "DMY" in up or "EURO" in up:
        return "DMY"
    if "YMD" in up:
        return "YMD"
    return "MDY"


def decode(text: str, tag: str) -> Parsed:
    """Decode ``text`` as PostgreSQL's DecodeDateTime does, raising the
    exact PostgreSQL error (``tag`` names the target type)."""
    try:
        return _decode(text, session_date_order())
    except _Fail as f:
        raise _error(f.kind, text, tag) from None


def _session_tz() -> _dt.tzinfo:
    from secantus.sql.datetimes import session_tzinfo

    session = _session()
    if session is None:
        return _dt.timezone.utc
    return session_tzinfo(session)


def zone_offset(local: _dt.datetime, tz: _dt.tzinfo) -> int:
    """The UTC offset (seconds east) ``tz`` has at naive wall clock ``local``,
    resolved as PostgreSQL's DetermineTimeZoneOffset does: in a spring-forward
    gap the BEFORE-transition offset, in a fall-back overlap the AFTER one."""
    o0 = local.replace(tzinfo=tz, fold=0).utcoffset() or _dt.timedelta(0)
    o1 = local.replace(tzinfo=tz, fold=1).utcoffset() or _dt.timedelta(0)
    if o0 == o1:
        return int(o0.total_seconds())

    def _valid(off: _dt.timedelta) -> bool:
        utc = (local - off).replace(tzinfo=_dt.timezone.utc)
        return utc.astimezone(tz).replace(tzinfo=None) == local

    if _valid(o0) and _valid(o1):  # overlap: prefer the later (standard) reading
        return int(o1.total_seconds())
    return int(o0.total_seconds())


def _today() -> tuple[int, int, int]:
    now = _dt.datetime.now(_session_tz())
    return now.year, now.month, now.day


def _fields_micros(p: Parsed) -> int:
    """Microseconds since 0001-01-01 00:00 (Python ordinal 1), astronomical."""
    days = gregorian_ordinal(p.year, p.month, p.day) - 1
    secs = p.hour * 3600 + p.minute * 60 + p.second
    return days * 86_400_000_000 + secs * 1_000_000 + p.micros


def _micros_fields(us: int) -> tuple[int, int, int, int, int, int, int]:
    days, rem = divmod(us, 86_400_000_000)
    y, m, d = ordinal_to_gregorian(days + 1)
    secs, micros = divmod(rem, 1_000_000)
    hh, secs = divmod(secs, 3600)
    mi, ss = divmod(secs, 60)
    return y, m, d, hh, mi, ss, micros


def _fill_relative(p: Parsed) -> None:
    if p.relative_day is not None:
        y, m, d = _today()
        o = gregorian_ordinal(y, m, d) + p.relative_day
        p.year, p.month, p.day = ordinal_to_gregorian(o)


#: PostgreSQL's timestamp range: 4714-11-24 BC .. 294276-12-31 AD.
_TS_MIN = _fields_micros(Parsed(year=-4713, month=11, day=24))
_TS_END = _fields_micros(Parsed(year=294277, month=1, day=1))
_DATE_MIN_ORD = gregorian_ordinal(-4713, 11, 24)
_DATE_END_ORD = gregorian_ordinal(5874898, 1, 1)


def _ymd_text(y: int, m: int, d: int) -> tuple[str, str]:
    if y <= 0:
        return f"{1 - y:04d}-{m:02d}-{d:02d}", " BC"
    return f"{y:04d}-{m:02d}-{d:02d}", ""


def parse_date_text(text: str) -> str:
    """A ``date`` literal as the canonical text the SQL layer stores."""
    p = decode(text, "date")
    if p.special == "epoch":
        return "1970-01-01"
    if p.special in ("infinity", "-infinity"):
        return p.special
    if p.special == "now":
        y, m, d = _today()
        return f"{y:04d}-{m:02d}-{d:02d}"
    _fill_relative(p)
    o = gregorian_ordinal(p.year, p.month, p.day)
    if not _DATE_MIN_ORD <= o < _DATE_END_ORD:
        raise DtParseError("22008", f'date out of range: "{text}"')
    body, era = _ymd_text(p.year, p.month, p.day)
    return body + era


def _pg_offset_text(secs: int) -> str:
    sign = "+" if secs >= 0 else "-"
    secs = abs(secs)
    hh, rem = divmod(secs, 3600)
    mm, ss = divmod(rem, 60)
    out = f"{sign}{hh:02d}"
    if mm or ss:
        out += f":{mm:02d}"
    if ss:
        out += f":{ss:02d}"
    return out


def _ts_text(us: int, offset: int | None) -> str:
    y, m, d, hh, mi, ss, micros = _micros_fields(us)
    body, era = _ymd_text(y, m, d)
    text = f"{body} {hh:02d}:{mi:02d}:{ss:02d}"
    if micros:
        text += "." + f"{micros:06d}".rstrip("0")
    if offset is not None:
        text += _pg_offset_text(offset)
    return text + era


def _in_python_range(us: int) -> bool:
    return 0 <= us < _fields_micros(Parsed(year=10000, month=1, day=1))


_PY_EPOCH = _dt.datetime(1, 1, 1)


def parse_timestamp_text(text: str, *, with_tz: bool) -> Any:
    """A ``timestamp`` / ``timestamptz`` literal as the value the SQL layer
    stores: a ``datetime`` (aware UTC for timestamptz) when Python can hold it,
    else the canonical wide text; ``infinity`` / ``-infinity`` sentinels."""
    tag = "timestamptz" if with_tz else "timestamp"
    p = decode(text, tag)
    if p.special in ("infinity", "-infinity"):
        return p.special
    if p.special == "epoch":
        if with_tz:
            return _dt.datetime(1970, 1, 1, tzinfo=_dt.timezone.utc)
        return _dt.datetime(1970, 1, 1)
    if p.special == "now":
        now = _dt.datetime.now(_dt.timezone.utc)
        return now if with_tz else now.astimezone(_session_tz()).replace(tzinfo=None)
    _fill_relative(p)
    local = _fields_micros(p)
    if not with_tz:
        us = local
        if not _TS_MIN <= us < _TS_END:
            raise DtParseError("22008", f'timestamp out of range: "{text}"')
        if _in_python_range(us):
            return _PY_EPOCH + _dt.timedelta(microseconds=us)
        return _ts_text(us, None)
    # No connection bound (the embedded API): hand back the wall clock naive,
    # and let the caller apply whatever zone its context carries.
    if (
        p.offset is None
        and p.zone is None
        and _session() is None
        and _TS_MIN <= local < _TS_END
        and _in_python_range(local)
    ):
        return _PY_EPOCH + _dt.timedelta(microseconds=local)
    if p.offset is not None:
        off = p.offset
    else:
        tz = p.zone if p.zone is not None else _session_tz()
        off = _offset_at(local, tz)
    us = local - off * 1_000_000
    if not _TS_MIN <= us < _TS_END:
        raise DtParseError("22008", f'timestamp out of range: "{text}"')
    if _in_python_range(us):
        return (_PY_EPOCH + _dt.timedelta(microseconds=us)).replace(tzinfo=_dt.timezone.utc)
    # Beyond Python's range: canonical text rendered in the session zone.
    stz = _session_tz()
    sess_off = _offset_at(us, stz)
    return _ts_text(us + sess_off * 1_000_000, sess_off)


def _offset_at(local_us: int, tz: _dt.tzinfo) -> int:
    if tz is _dt.timezone.utc:
        return 0
    if isinstance(tz, _dt.timezone):
        off = tz.utcoffset(None)
        return int(off.total_seconds()) if off is not None else 0
    # Clamp to Python's range: outside it the zone's offset nearest in time.
    lo = _fields_micros(Parsed(year=1, month=1, day=2))
    hi = _fields_micros(Parsed(year=9999, month=12, day=30))
    clamped = min(max(local_us, lo), hi)
    return zone_offset(_PY_EPOCH + _dt.timedelta(microseconds=clamped), tz)

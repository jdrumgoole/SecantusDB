"""``timelib_strtotime``: the scanner loop, the rule actions and their helpers,
ported from timelib 2022.13 ``parse_date.re`` (MIT; see ``__init__.py``).

re2c semantics are reproduced exactly: at each position the LONGEST match over
all rules wins, a tie goes to the rule listed first, and the input is the
trimmed string followed by NUL padding -- several rules (``meridian``,
``datenoyear``) consume the terminating NUL as part of a token. Python's ``re``
is leftmost-first rather than longest, so each rule's longest match is found by
asking ``fullmatch`` for every candidate end, longest first. Correctness over
speed: date strings are short.
"""

from __future__ import annotations

import functools
import re

from secantus.timelib import Message, Parsed, Time
from secantus.timelib.consts import (
    ERR_TZID_NOT_FOUND,
    I64_MAX,
    I64_MIN,
    SPECIAL_DAY_OF_WEEK_IN_MONTH,
    SPECIAL_FIRST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH,
    SPECIAL_WEEKDAY,
    UNSET,
    ZONETYPE_ABBR,
    ZONETYPE_OFFSET,
    wrap_i64,
)
from secantus.timelib.patterns import compiled_rules
from secantus.timelib.update import daynr_from_weeknr, days_in_month
from secantus.timelib.zones import TIMEZONE_MAP

ERR_DOUBLE_TZ = 0x201
ERR_DOUBLE_TIME = 0x203
ERR_DOUBLE_DATE = 0x204
ERR_UNEXPECTED_CHARACTER = 0x205
ERR_EMPTY_STRING = 0x206
ERR_UNEXPECTED_DATA = 0x207
ERR_NUMBER_OUT_OF_RANGE = 0x226
WARN_DOUBLE_TZ = 0x101
WARN_INVALID_TIME = 0x102
WARN_INVALID_DATE = 0x103

#: ``MAX_ABBR_LEN``: ``_POSIX_TZNAME_MAX``, 6 on Linux and macOS alike.
MAX_ABBR_LEN = 6
#: NUL bytes after the string; more than any rule looks ahead.
PADDING = 64

# The relunit kinds of `timelib_relunit_lookup`.
MICROSEC = 9
SECOND = 1
MINUTE = 2
HOUR = 3
DAY = 4
MONTH = 5
YEAR = 6
WEEKDAY = 7
SPECIAL = 8

TIME_PART_DONT_KEEP = False
TIME_PART_KEEP = True

_MU = "µ".encode()

#: ``timelib_relunit_lookup``: (name, unit, multiplier).
RELUNITS: tuple[tuple[bytes, int, int], ...] = (
    (b"ms", MICROSEC, 1000),
    (b"msec", MICROSEC, 1000),
    (b"msecs", MICROSEC, 1000),
    (b"millisecond", MICROSEC, 1000),
    (b"milliseconds", MICROSEC, 1000),
    (_MU + b"s", MICROSEC, 1),
    (b"usec", MICROSEC, 1),
    (b"usecs", MICROSEC, 1),
    (_MU + b"sec", MICROSEC, 1),
    (_MU + b"secs", MICROSEC, 1),
    (b"microsecond", MICROSEC, 1),
    (b"microseconds", MICROSEC, 1),
    (b"sec", SECOND, 1),
    (b"secs", SECOND, 1),
    (b"second", SECOND, 1),
    (b"seconds", SECOND, 1),
    (b"min", MINUTE, 1),
    (b"mins", MINUTE, 1),
    (b"minute", MINUTE, 1),
    (b"minutes", MINUTE, 1),
    (b"hour", HOUR, 1),
    (b"hours", HOUR, 1),
    (b"day", DAY, 1),
    (b"days", DAY, 1),
    (b"week", DAY, 7),
    (b"weeks", DAY, 7),
    (b"fortnight", DAY, 14),
    (b"fortnights", DAY, 14),
    (b"forthnight", DAY, 14),
    (b"forthnights", DAY, 14),
    (b"month", MONTH, 1),
    (b"months", MONTH, 1),
    (b"year", YEAR, 1),
    (b"years", YEAR, 1),
    (b"mondays", WEEKDAY, 1),
    (b"monday", WEEKDAY, 1),
    (b"mon", WEEKDAY, 1),
    (b"tuesdays", WEEKDAY, 2),
    (b"tuesday", WEEKDAY, 2),
    (b"tue", WEEKDAY, 2),
    (b"wednesdays", WEEKDAY, 3),
    (b"wednesday", WEEKDAY, 3),
    (b"wed", WEEKDAY, 3),
    (b"thursdays", WEEKDAY, 4),
    (b"thursday", WEEKDAY, 4),
    (b"thu", WEEKDAY, 4),
    (b"fridays", WEEKDAY, 5),
    (b"friday", WEEKDAY, 5),
    (b"fri", WEEKDAY, 5),
    (b"saturdays", WEEKDAY, 6),
    (b"saturday", WEEKDAY, 6),
    (b"sat", WEEKDAY, 6),
    (b"sundays", WEEKDAY, 0),
    (b"sunday", WEEKDAY, 0),
    (b"sun", WEEKDAY, 0),
    (b"weekday", SPECIAL, SPECIAL_WEEKDAY),
    (b"weekdays", SPECIAL, SPECIAL_WEEKDAY),
)

#: ``timelib_reltext_lookup``: (name, behavior, value).
RELTEXT: tuple[tuple[bytes, int, int], ...] = (
    (b"first", 0, 1),
    (b"next", 0, 1),
    (b"second", 0, 2),
    (b"third", 0, 3),
    (b"fourth", 0, 4),
    (b"fifth", 0, 5),
    (b"sixth", 0, 6),
    (b"seventh", 0, 7),
    (b"eight", 0, 8),
    (b"eighth", 0, 8),
    (b"ninth", 0, 9),
    (b"tenth", 0, 10),
    (b"eleventh", 0, 11),
    (b"twelfth", 0, 12),
    (b"last", 0, -1),
    (b"previous", 0, -1),
    (b"this", 1, 0),
)

#: ``timelib_month_lookup``.
MONTHS: tuple[tuple[bytes, int], ...] = (
    (b"jan", 1),
    (b"feb", 2),
    (b"mar", 3),
    (b"apr", 4),
    (b"may", 5),
    (b"jun", 6),
    (b"jul", 7),
    (b"aug", 8),
    (b"sep", 9),
    (b"sept", 9),
    (b"oct", 10),
    (b"nov", 11),
    (b"dec", 12),
    (b"i", 1),
    (b"ii", 2),
    (b"iii", 3),
    (b"iv", 4),
    (b"v", 5),
    (b"vi", 6),
    (b"vii", 7),
    (b"viii", 8),
    (b"ix", 9),
    (b"x", 10),
    (b"xi", 11),
    (b"xii", 12),
    (b"january", 1),
    (b"february", 2),
    (b"march", 3),
    (b"april", 4),
    (b"may", 5),
    (b"june", 6),
    (b"july", 7),
    (b"august", 8),
    (b"september", 9),
    (b"october", 10),
    (b"november", 11),
    (b"december", 12),
)


def _ascii_lower(b: bytes) -> bytes:
    """``eq_ignore_ascii_case``'s fold: ASCII letters only, bytes untouched."""
    return b.lower()  # bytes.lower() folds ASCII only


# --- the matcher -------------------------------------------------------------


@functools.lru_cache(maxsize=1)
def _rules() -> list[tuple[str, re.Pattern[bytes]]]:
    return compiled_rules()


def longest_match(buf: bytes, pos: int) -> tuple[str, int] | None:
    """re2c's choice at ``pos``: the longest match over every rule, the
    earliest rule on a tie. ``None`` only if nothing matches, which the ``any``
    rule makes impossible inside the buffer."""
    best: tuple[int, str] | None = None
    end_limit = len(buf)
    for rule, pat in _rules():
        m = pat.match(buf, pos)
        if m is None:
            continue
        length = m.end() - pos
        # Some match exists; find the longest by trying every longer end.
        floor = max(m.end(), pos + (best[0] if best else 0) + 1)
        for end in range(end_limit, floor - 1, -1):
            if end > m.end() and pat.fullmatch(buf, pos, end):
                length = end - pos
                break
        if best is None or length > best[0]:
            best = (length, rule)
    return None if best is None else (best[1], best[0])


# --- the scanner -------------------------------------------------------------


class Tok:
    """A token copy with C-string semantics: reading past the end yields NUL."""

    __slots__ = ("b", "p")

    def __init__(self, b: bytes, p: int = 0) -> None:
        self.b = b
        self.p = p

    def at(self, i: int) -> int:
        return self.b[i] if 0 <= i < len(self.b) else 0

    def cur(self) -> int:
        return self.at(self.p)


def _isdigit(c: int) -> bool:
    return 0x30 <= c <= 0x39


def _isalpha(c: int) -> bool:
    return 0x41 <= c <= 0x5A or 0x61 <= c <= 0x7A


def is_c_space(c: int) -> bool:
    return c in (0x20, 0x09, 0x0A, 0x0B, 0x0C, 0x0D)


_SP, _TAB = 0x20, 0x09


def strtotime(data: bytes) -> Parsed:
    """``timelib_strtotime``."""
    s = _Scanner()
    if not data:
        # `tok` is NULL here, so the position and character are both 0.
        s.errors.append(Message(ERR_EMPTY_STRING, 0, 0, "Empty string"))
        return Parsed(s.time, s.errors, s.warnings)
    start, e = 0, len(data) - 1
    while is_c_space(data[start]) and start < e:
        start += 1
    while is_c_space(data[e]) and e > start:
        e -= 1
    trimmed = data[start : e + 1]
    s.length = len(trimmed)
    s.buf = trimmed + b"\x00" * PADDING

    cursor = 0
    while True:
        s.tok = cursor
        # YYFILL: the buffer is the string plus YYMAXFILL bytes, so the scan
        # stops once the token would start past the terminating NUL.
        if s.tok > s.length:
            break
        found = longest_match(s.buf, cursor)
        if found is None:
            break
        rule, length = found
        cursor += max(length, 1)
        s.action(rule, s.buf[s.tok : cursor])

    # "funky checking" whether the parsed time and date were valid.
    if s.time.have_time and not valid_time(s.time.h, s.time.i, s.time.s):
        s.add_warning(WARN_INVALID_TIME, "The parsed time was invalid")
    if s.time.have_date and not valid_date(s.time.y, s.time.m, s.time.d):
        s.add_warning(WARN_INVALID_DATE, "The parsed date was invalid")
    return Parsed(s.time, s.errors, s.warnings)


class _Scanner:
    def __init__(self) -> None:
        self.buf = b""
        self.length = 0
        self.tok = 0
        self.time = Time()
        self.errors: list[Message] = []
        self.warnings: list[Message] = []

    def char_at_tok(self) -> int:
        return self.buf[self.tok] if self.tok < len(self.buf) else 0

    def add_error(self, code: int, message: str) -> None:
        self.errors.append(Message(code, self.tok, self.char_at_tok(), message))

    def add_warning(self, code: int, message: str) -> None:
        self.warnings.append(Message(code, self.tok, self.char_at_tok(), message))

    # The TIMELIB_* state macros. Each `have_*` returns False where the C
    # macro `return TIMELIB_ERROR`s out of the action.
    def have_time(self) -> bool:
        t = self.time
        if t.have_time != 0:
            self.add_error(ERR_DOUBLE_TIME, "Double time specification")
            return False
        t.have_time = 1
        t.h = t.i = t.s = t.us = 0
        return True

    def unhave_time(self) -> None:
        t = self.time
        t.have_time = 0
        t.h = t.i = t.s = t.us = 0

    def have_date(self) -> bool:
        if self.time.have_date:
            self.add_error(ERR_DOUBLE_DATE, "Double date specification")
            return False
        self.time.have_date = True
        return True

    def unhave_date(self) -> None:
        t = self.time
        t.have_date = False
        t.d = t.m = t.y = 0

    def have_relative(self) -> None:
        self.time.have_relative = True

    def have_weekday_relative(self) -> None:
        self.time.have_relative = True
        self.time.relative.have_weekday_relative = True

    def have_special_relative(self) -> None:
        self.time.have_relative = True
        self.time.relative.have_special_relative = True

    def have_tz(self) -> bool:
        t = self.time
        if t.have_zone != 0:
            if t.have_zone > 1:
                self.add_error(ERR_DOUBLE_TZ, "Double timezone specification")
            else:
                self.add_warning(WARN_DOUBLE_TZ, "Double timezone specification")
            t.have_zone += 1
            return False
        t.have_zone += 1
        return True

    def action(self, rule: str, token: bytes) -> None:  # noqa: C901, PLR0912, PLR0915
        """The action block of one rule. ``token`` is the matched text, which
        the C code copies into a NUL-terminated string and walks with ``ptr``."""
        t = Tok(token)
        tm = self.time
        r = tm.relative
        if rule == "Yesterday":
            self.have_relative()
            self.unhave_time()
            r.d = -1
        elif rule == "Now":
            pass
        elif rule == "Noon":
            self.unhave_time()
            if not self.have_time():
                return
            tm.h = 12
        elif rule == "MidnightToday":
            self.unhave_time()
        elif rule == "Tomorrow":
            self.have_relative()
            self.unhave_time()
            r.d = 1
        elif rule in ("Timestamp", "TimestampMs"):
            self.have_relative()
            self.unhave_date()
            self.unhave_time()
            if not self.have_tz():
                return
            is_negative = t.at(1) == ord("-")
            i = self.get_signed_nr(t, 24)
            us = 0
            if rule == "TimestampMs":
                before = t.p
                us = self.get_signed_nr(t, 6)
                us *= 10 ** (7 - (t.p - before))
                if is_negative:
                    us *= -1
            tm.y, tm.m, tm.d = 1970, 1, 1
            tm.h = tm.i = tm.s = tm.us = 0
            r.s += i
            if rule == "TimestampMs":
                r.us = us
            tm.zone_type = ZONETYPE_OFFSET
            tm.z = 0
            tm.dst = 0
        elif rule == "FirstLastDayOf":
            self.have_relative()
            self.unhave_time()
            r.first_last_day_of = (
                SPECIAL_LAST_DAY_OF_MONTH if t.cur() in b"lL" else SPECIAL_FIRST_DAY_OF_MONTH
            )
        elif rule == "BackFrontOf":
            self.unhave_time()
            if not self.have_time():
                return
            if t.cur() == ord("b"):
                tm.h = get_nr(t, 2)
                tm.i = 15
            else:
                tm.h = get_nr(t, 2) - 1
                tm.i = 45
            if t.cur() != 0:
                eat_spaces(t)
                tm.h += meridian(t, tm.h)
        elif rule == "WeekdayOf":
            self.have_relative()
            self.have_special_relative()
            i, behavior = get_relative_text(t)
            eat_spaces(t)
            if i > 0:
                r.special_type = SPECIAL_DAY_OF_WEEK_IN_MONTH
                self.set_relative(t, i, 1, TIME_PART_DONT_KEEP)
            else:
                r.special_type = SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH
                self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP)
        elif rule == "Time12":
            if not self.have_time():
                return
            tm.h = get_nr(t, 2)
            if t.cur() in b":.":
                tm.i = get_nr(t, 2)
                if t.cur() in b":.":
                    tm.s = get_nr(t, 2)
            eat_spaces(t)
            tm.h += meridian(t, tm.h)
        elif rule == "MssqlTime":
            if not self.have_time():
                return
            tm.h = get_nr(t, 2)
            tm.i = get_nr(t, 2)
            if t.cur() in b":.":
                tm.s = get_nr(t, 2)
                if t.cur() in b":.":
                    tm.us = get_frac_nr(t)
            eat_spaces(t)
            tm.h += meridian(t, tm.h)
        elif rule == "Time24":
            if not self.have_time():
                return
            tm.h = get_nr(t, 2)
            if t.cur() in b":.":
                tm.i = get_nr(t, 2)
                if t.cur() in b":.":
                    tm.s = get_nr(t, 2)
                    if t.cur() == ord("."):
                        tm.us = get_frac_nr(t)
            if t.cur() != 0:
                self.zone(t)
        elif rule == "GnuNoColon":
            if tm.have_time == 0:
                tm.h = get_nr(t, 2)
                tm.i = get_nr(t, 2)
                tm.s = 0
                tm.have_time += 1
            elif tm.have_time == 1:
                tm.y = get_nr(t, 4)
                tm.have_time += 1
            else:
                self.add_error(ERR_DOUBLE_TIME, "Double time specification")
        elif rule == "Iso8601NoColon":
            if not self.have_time():
                return
            tm.h = get_nr(t, 2)
            tm.i = get_nr(t, 2)
            tm.s = get_nr(t, 2)
            if t.cur() != 0:
                self.zone(t)
        elif rule == "American":
            if not self.have_date():
                return
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
            if t.cur() == ord("/"):
                y, n = get_nr_ex(t, 4)
                tm.y = process_year(y, n)
        elif rule == "IsoDate4":
            if not self.have_date():
                return
            tm.y = self.get_signed_nr(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
        elif rule == "IsoDate2":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
            tm.y = process_year(y, n)
        elif rule == "IsoDateX":
            if not self.have_date():
                return
            tm.y = self.get_signed_nr(t, 19)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
        elif rule == "GnuDateShorter":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = 1
            tm.y = process_year(y, n)
        elif rule == "GnuDateShort":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
            tm.y = process_year(y, n)
        elif rule == "DateFull":
            if not self.have_date():
                return
            tm.d = get_nr(t, 2)
            skip_day_suffix(t)
            tm.m = get_month(t)
            y, n = get_nr_ex(t, 4)
            tm.y = process_year(y, n)
        elif rule == "PointedDate4":
            if not self.have_date():
                return
            tm.d = get_nr(t, 2)
            tm.m = get_nr(t, 2)
            tm.y = get_nr(t, 4)
        elif rule == "PointedDate2":
            if not self.have_date():
                return
            tm.d = get_nr(t, 2)
            tm.m = get_nr(t, 2)
            y, n = get_nr_ex(t, 2)
            tm.y = process_year(y, n)
        elif rule == "DateNoDay":
            if not self.have_date():
                return
            tm.m = get_month(t)
            y, n = get_nr_ex(t, 4)
            tm.d = 1
            tm.y = process_year(y, n)
        elif rule == "DateNoDayRev":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.m = get_month(t)
            tm.d = 1
            tm.y = process_year(y, n)
        elif rule == "DateTextual":
            if not self.have_date():
                return
            tm.m = get_month(t)
            tm.d = get_nr(t, 2)
            y, n = get_nr_ex(t, 4)
            tm.y = process_year(y, n)
        elif rule == "DateNoYearRev":
            if not self.have_date():
                return
            tm.d = get_nr(t, 2)
            skip_day_suffix(t)
            tm.m = get_month(t)
        elif rule == "DateNoColon":
            if not self.have_date():
                return
            tm.y = get_nr(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
        elif rule == "XmlRpc":
            if not self.have_time() or not self.have_date():
                return
            tm.y = get_nr(t, 4)
            tm.m = get_nr(t, 2)
            tm.d = get_nr(t, 2)
            tm.h = get_nr(t, 2)
            tm.i = get_nr(t, 2)
            tm.s = get_nr(t, 2)
            if t.cur() == ord("."):
                tm.us = get_frac_nr(t)
                if t.cur() != 0:
                    self.zone(t)
        elif rule == "PgYdotd":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.d = get_nr(t, 3)
            tm.m = 1
            tm.y = process_year(y, n)
        elif rule in ("IsoWeekDay", "IsoWeek"):
            if not self.have_date():
                return
            self.have_relative()
            tm.y = get_nr(t, 4)
            w = get_nr(t, 2)
            d = get_nr(t, 1) if rule == "IsoWeekDay" else 1
            tm.m = 1
            tm.d = 1
            r.d = daynr_from_weeknr(tm.y, w, d)
        elif rule == "PgTextShort":
            if not self.have_date():
                return
            tm.m = get_month(t)
            tm.d = get_nr(t, 2)
            y, n = get_nr_ex(t, 4)
            tm.y = process_year(y, n)
        elif rule == "PgTextReverse":
            if not self.have_date():
                return
            y, n = get_nr_ex(t, 4)
            tm.m = get_month(t)
            tm.d = get_nr(t, 2)
            tm.y = process_year(y, n)
        elif rule == "Clf":
            if not self.have_time() or not self.have_date():
                return
            tm.d = get_nr(t, 2)
            tm.m = get_month(t)
            tm.y = get_nr(t, 4)
            tm.h = get_nr(t, 2)
            tm.i = get_nr(t, 2)
            tm.s = get_nr(t, 2)
            eat_spaces(t)
            self.zone(t)
        elif rule == "Year4":
            tm.y = get_nr(t, 4)
        elif rule == "Ago":
            r.y, r.m, r.d, r.h, r.i, r.s = -r.y, -r.m, -r.d, -r.h, -r.i, -r.s
            r.weekday = -r.weekday
            if r.weekday == 0:
                r.weekday = -7
            if r.have_special_relative and r.special_type == SPECIAL_WEEKDAY:
                r.special_amount = -r.special_amount
        elif rule == "DayText":
            self.have_relative()
            self.have_weekday_relative()
            self.unhave_time()
            found = lookup_relunit(t)
            if found is not None:
                r.weekday = found[2]
            if r.weekday_behavior != 2:
                r.weekday_behavior = 1
        elif rule == "RelativeTextWeek":
            self.have_relative()
            while t.cur() != 0:
                i, behavior = get_relative_text(t)
                eat_spaces(t)
                self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP)
                r.weekday_behavior = 2
                if not r.have_weekday_relative:
                    self.have_weekday_relative()
                    r.weekday = 1
        elif rule == "RelativeText":
            self.have_relative()
            while t.cur() != 0:
                i, behavior = get_relative_text(t)
                eat_spaces(t)
                self.set_relative(t, i, behavior, TIME_PART_DONT_KEEP)
        elif rule == "MonthText":
            if not self.have_date():
                return
            tm.m = lookup_month(t)
        elif rule == "Tz":
            if not self.have_tz():
                return
            eat_spaces(t)
            self.zone(t)
        elif rule in ("DateShortWithTime12", "DateShortWithTime24"):
            if not self.have_date():
                return
            tm.m = get_month(t)
            tm.d = get_nr(t, 2)
            if not self.have_time():
                return
            tm.h = get_nr(t, 2)
            tm.i = get_nr(t, 2)
            if rule == "DateShortWithTime12":
                if t.cur() in b":.":
                    tm.s = get_nr(t, 2)
                    if t.cur() == ord("."):
                        tm.us = get_frac_nr(t)
                tm.h += meridian(t, tm.h)
            else:
                if t.cur() == ord(":"):
                    tm.s = get_nr(t, 2)
                    if t.cur() == ord("."):
                        tm.us = get_frac_nr(t)
                if t.cur() != 0:
                    self.zone(t)
        elif rule == "Relative":
            self.have_relative()
            while t.cur() != 0:
                i = self.get_signed_nr(t, 24)
                eat_spaces(t)
                self.set_relative(t, i, 1, TIME_PART_KEEP)
        elif rule in ("DotComma", "Space", "NulNewline"):
            pass
        elif rule == "Any":
            self.add_error(ERR_UNEXPECTED_CHARACTER, "Unexpected character")
        else:  # pragma: no cover -- every rule is listed above
            raise AssertionError(rule)

    def zone(self, t: Tok) -> None:
        """``timelib_parse_zone`` at the call sites: store the offset and report
        a zone that is not in the database."""
        z, not_found = parse_zone(t, self.time)
        self.time.z = z
        if not_found:
            self.add_error(ERR_TZID_NOT_FOUND, "The timezone could not be found in the database")

    def get_signed_nr(self, t: Tok, max_length: int) -> int:
        """``timelib_get_signed_nr``."""
        while not _isdigit(t.cur()) and t.cur() not in (0x2B, 0x2D):
            if t.cur() == 0:
                self.add_error(ERR_UNEXPECTED_DATA, "Found unexpected data")
                return 0
            t.p += 1
        negative = False
        while t.cur() in (0x2B, 0x2D):
            if t.cur() == 0x2D:
                negative = not negative
            t.p += 1
        while not _isdigit(t.cur()):
            if t.cur() == 0:
                self.add_error(ERR_UNEXPECTED_DATA, "Found unexpected data")
                return 0
            t.p += 1
        begin = t.p
        while _isdigit(t.cur()) and t.p - begin < max_length:
            t.p += 1
        value = int(t.b[begin : t.p])
        if negative:
            value = -value
        if not I64_MIN <= value <= I64_MAX:
            self.add_error(ERR_NUMBER_OUT_OF_RANGE, "Number out of range")
            return 0
        return value

    def set_relative(self, t: Tok, amount: int, behavior: int, keep_time: bool) -> None:
        """``timelib_set_relative``."""
        found = lookup_relunit(t)
        if found is None:
            return
        _, unit, multiplier = found
        r = self.time.relative
        attr = {
            MICROSEC: "us",
            SECOND: "s",
            MINUTE: "i",
            HOUR: "h",
            DAY: "d",
            MONTH: "m",
            YEAR: "y",
        }.get(unit)
        if attr is not None:
            # add_with_overflow: __builtin_saddll_overflow on the product.
            total = getattr(r, attr) + wrap_i64(amount * multiplier)
            if not I64_MIN <= total <= I64_MAX:
                self.add_error(ERR_NUMBER_OUT_OF_RANGE, "Number out of range")
            setattr(r, attr, wrap_i64(total))
            return
        if unit == WEEKDAY:
            self.have_weekday_relative()
            if not keep_time:
                self.unhave_time()
            r.d += (amount - 1 if amount > 0 else amount) * 7
            r.weekday = multiplier
            r.weekday_behavior = behavior
        elif unit == SPECIAL:
            self.have_special_relative()
            if not keep_time:
                self.unhave_time()
            r.special_type = multiplier
            r.special_amount = amount


# --- helpers (free functions in parse_date.re) -------------------------------


def process_year(y: int, length: int) -> int:
    """``TIMELIB_PROCESS_YEAR``."""
    if y == UNSET or length >= 4:
        return y
    if y < 100:
        return y + 2000 if y < 70 else y + 1900
    return y


def meridian(t: Tok, h: int) -> int:
    """``timelib_meridian``."""
    retval = 0
    while t.cur() not in b"AaPp":
        # strchr("AaPp", c) also finds the terminating NUL, which ends the
        # loop at the end of the token.
        if t.cur() == 0:
            break
        t.p += 1
    if t.cur() in b"aA" and t.cur() != 0:
        if h == 12:
            retval = -12
    elif h != 12:
        retval = 12
    t.p += 1
    if t.cur() == ord("."):
        t.p += 1
    if t.cur() in b"Mm" and t.cur() != 0:
        t.p += 1
    if t.cur() == ord("."):
        t.p += 1
    return retval


def get_nr_ex(t: Tok, max_length: int) -> tuple[int, int]:
    """``timelib_get_nr_ex``: the number and how many digits it had."""
    while not _isdigit(t.cur()):
        if t.cur() == 0:
            return UNSET, 0
        t.p += 1
    begin = t.p
    while _isdigit(t.cur()) and t.p - begin < max_length:
        t.p += 1
    return int(t.b[begin : t.p]), t.p - begin


def get_nr(t: Tok, max_length: int) -> int:
    """``timelib_get_nr``."""
    return get_nr_ex(t, max_length)[0]


def skip_day_suffix(t: Tok) -> None:
    """``timelib_skip_day_suffix``."""
    if is_c_space(t.cur()):
        return
    two = bytes([t.cur(), t.at(t.p + 1)]).lower()
    if two in (b"nd", b"rd", b"st", b"th"):
        t.p += 2


def get_frac_nr(t: Tok) -> int:
    """``timelib_get_frac_nr``: the fraction as microseconds."""
    while t.cur() not in (0x2E, 0x3A) and not _isdigit(t.cur()):
        if t.cur() == 0:
            return UNSET
        t.p += 1
    begin = t.p
    while t.cur() in (0x2E, 0x3A) or _isdigit(t.cur()):
        t.p += 1
    end = t.p
    # strtod of everything after the first character, stopping at the first
    # byte that is not part of a number.
    numeric = re.match(rb"[0-9.]*", t.b[begin + 1 : end]).group(0)  # type: ignore[union-attr]
    try:
        value = float(numeric)
    except ValueError:
        value = 0.0
    return int(value * 10.0 ** (7 - (end - begin)))


def lookup_relative_text(t: Tok) -> tuple[int, int]:
    """``timelib_lookup_relative_text``: (value, behavior)."""
    begin = t.p
    while _isalpha(t.cur()):
        t.p += 1
    word = t.b[begin : t.p].lower()
    value, behavior = 0, 0
    for name, b, v in RELTEXT:
        if word == name:
            value, behavior = v, b
    return value, behavior


def get_relative_text(t: Tok) -> tuple[int, int]:
    """``timelib_get_relative_text``: (value, behavior)."""
    while t.cur() in (_SP, _TAB, 0x2D, 0x2F):
        t.p += 1
    return lookup_relative_text(t)


def lookup_month(t: Tok) -> int:
    """``timelib_lookup_month``."""
    begin = t.p
    while _isalpha(t.cur()):
        t.p += 1
    word = t.b[begin : t.p].lower()
    value = 0
    for name, v in MONTHS:
        if word == name:
            value = v
    return value


def get_month(t: Tok) -> int:
    """``timelib_get_month``."""
    while t.cur() in (_SP, _TAB, 0x2D, 0x2E, 0x2F):
        t.p += 1
    return lookup_month(t)


def eat_spaces(t: Tok) -> None:
    """``timelib_eat_spaces``, NBSP and NNBSP included."""
    while True:
        if t.cur() in (_SP, _TAB):
            t.p += 1
        elif t.cur() == 0xE2 and t.at(t.p + 1) == 0x80 and t.at(t.p + 2) == 0xAF:
            t.p += 3
        elif t.cur() == 0xC2 and t.at(t.p + 1) == 0xA0:
            t.p += 2
        else:
            break


_RELUNIT_STOP = frozenset(b"\x00 ,\t;:/.-()")


def lookup_relunit(t: Tok) -> tuple[bytes, int, int] | None:
    """``timelib_lookup_relunit``: the first table entry matching the word."""
    begin = t.p
    while t.cur() not in _RELUNIT_STOP:
        t.p += 1
    word = t.b[begin : t.p].lower()
    for entry in RELUNITS:
        if word == entry[0].lower():
            return entry
    return None


def _first_by_name() -> dict[bytes, tuple[int, int]]:
    out: dict[bytes, tuple[int, int]] = {}
    for name, dst, gmtoffset, _full in TIMEZONE_MAP:
        out.setdefault(name.encode().lower(), (dst, gmtoffset))
    return out


_ZONES = _first_by_name()


def abbr_search(word: bytes) -> tuple[int, int] | None:
    """``abbr_search`` with ``gmtoffset == -1``: (dst, gmtoffset) of the first
    entry with this name."""
    w = word.lower()
    if w in (b"utc", b"gmt"):
        return 0, 0
    return _ZONES.get(w)


def lookup_abbr(t: Tok) -> tuple[int, int, bytes, bool]:
    """``timelib_lookup_abbr``: (offset, dst, word, found)."""
    begin = t.p
    while True:
        c = t.cur()
        if _isdigit(c) or _isalpha(c) or c in (0x2F, 0x5F, 0x2D, 0x2B):
            t.p += 1
        else:
            break
    word = t.b[begin : t.p]
    if len(word) < MAX_ABBR_LEN:
        e = abbr_search(word)
        if e is not None:
            dst, gmtoffset = e
            return gmtoffset - dst * 3600, dst, word, True
    return 0, 0, word, False


def _strtol_prefix(b: bytes) -> int:
    m = re.match(rb"[0-9]*", b)
    digits = m.group(0) if m else b""
    return int(digits) if digits else 0


def parse_tz_cor(t: Tok) -> tuple[int, bool]:
    """``timelib_parse_tz_cor``: (seconds, not_found)."""
    begin = t.p
    while _isdigit(t.cur()) or t.cur() == 0x3A:
        t.p += 1
    b = t.b[begin : t.p]

    def at(i: int) -> int:
        return b[i] if i < len(b) else 0

    def hour(s: bytes) -> int:
        return _strtol_prefix(s) * 3600

    def mins(s: bytes) -> int:
        return _strtol_prefix(s) * 60

    n = len(b)
    if n in (1, 2):
        return hour(b), False
    if n in (3, 4):
        if at(1) == 0x3A:
            return hour(b) + mins(b[2:]), False
        if at(2) == 0x3A:
            return hour(b) + mins(b[3:]), False
        tmp = _strtol_prefix(b)
        return (tmp // 100) * 3600 + (tmp % 100) * 60, False
    if n == 5 and at(2) == 0x3A:
        return hour(b) + mins(b[3:]), False
    if n == 6:
        tmp = _strtol_prefix(b)
        return (tmp // 10000) * 3600 + ((tmp // 100) % 100) * 60 + tmp % 100, False
    if n == 8 and at(2) == 0x3A and at(5) == 0x3A:
        return hour(b) + mins(b[3:]) + _strtol_prefix(b[6:]), False
    return 0, True


def parse_zone(t: Tok, time: Time) -> tuple[int, bool]:
    """``timelib_parse_zone`` with mongod's ``tz_get_wrapper``, which never
    finds a zone identifier. Returns (offset, not_found)."""
    while t.cur() in (_SP, _TAB, 0x28):
        t.p += 1
    if (
        t.cur() == ord("G")
        and t.at(t.p + 1) == ord("M")
        and t.at(t.p + 2) == ord("T")
        and t.at(t.p + 3) in (0x2B, 0x2D)
    ):
        t.p += 3
    if t.cur() in (0x2B, 0x2D):
        negative = t.cur() == 0x2D
        t.p += 1
        time.zone_type = ZONETYPE_OFFSET
        time.dst = 0
        v, not_found = parse_tz_cor(t)
        result = (-v if negative else v), not_found
    else:
        offset, dst, word, found = lookup_abbr(t)
        if found:
            time.zone_type = ZONETYPE_ABBR
            time.dst = dst
            time.tz_abbr = word.decode("latin-1").upper()
        result = offset, not found
    while t.cur() == 0x29:
        t.p += 1
    return result


def valid_time(h: int, i: int, s: int) -> bool:
    """``timelib_valid_time``."""
    return 0 <= h <= 23 and 0 <= i <= 59 and 0 <= s <= 59


def valid_date(y: int, m: int, d: int) -> bool:
    """``timelib_valid_date``."""
    return 1 <= m <= 12 and 1 <= d <= days_in_month(y, m)

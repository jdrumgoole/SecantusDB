"""``timelib_parse_from_format_with_map`` (timelib 2022.13 ``parse_date.re``,
MIT; see ``__init__.py``), with the configuration mongod passes it: the
``kDateFromStringFormatMap`` of ``date_time_support.cpp`` and ``%`` as the
required prefix. This is ``$dateFromString`` with a ``format``.

Like the free-form port it is literal: the loop keeps going after an error,
and every error and warning carries the position and character timelib would
report, because mongod prints all of them.
"""

from __future__ import annotations

from secantus.timelib import Message, Parsed, Time
from secantus.timelib.consts import (
    ERR_TZID_NOT_FOUND,
    I64_MAX,
    I64_MIN,
    UNSET,
    ZONETYPE_OFFSET,
    wrap_i64,
)
from secantus.timelib.scan import (
    Tok,
    get_nr,
    get_nr_ex,
    lookup_month,
    parse_zone,
    valid_date,
    valid_time,
)
from secantus.timelib.update import date_from_isodate, do_normalize

ERR_UNEXPECTED_DATA = 0x207
ERR_NO_TWO_DIGIT_DAY = 0x209
ERR_NO_THREE_DIGIT_DAY_OF_YEAR = 0x20A
ERR_NO_TWO_DIGIT_MONTH = 0x20B
ERR_NO_TEXTUAL_MONTH = 0x20C
ERR_NO_FOUR_DIGIT_YEAR = 0x20E
ERR_NO_TWO_DIGIT_HOUR = 0x20F
ERR_MERIDIAN_BEFORE_HOUR = 0x211
ERR_NO_TWO_DIGIT_MINUTE = 0x213
ERR_NO_TWO_DIGIT_SECOND = 0x214
ERR_NO_THREE_DIGIT_MILLISECOND = 0x21C
ERR_TRAILING_DATA = 0x21A
ERR_DATA_MISSING = 0x21B
ERR_FORMAT_LITERAL_MISMATCH = 0x224
ERR_MIX_ISO_WITH_NATURAL = 0x225
ERR_WRONG_FORMAT_SEP = 0x219
ERR_NO_FOUR_DIGIT_YEAR_ISO = 0x21D
ERR_NO_TWO_DIGIT_WEEK = 0x21E
ERR_INVALID_WEEK = 0x21F
ERR_NO_DAY_OF_WEEK = 0x220
ERR_INVALID_DAY_OF_WEEK = 0x221
ERR_INVALID_TZ_OFFSET = 0x223
WARN_INVALID_TIME = 0x102
WARN_INVALID_DATE = 0x103

#: ``timelib_lookup_format`` over ``kDateFromStringFormatMap``; anything else
#: is a literal.
_FORMAT_MAP = {
    ord("b"): "TextualMonth",
    ord("B"): "TextualMonth",
    ord("d"): "DayTwoDigit",
    ord("G"): "YearIso",
    ord("H"): "Hour24",
    ord("j"): "DayOfYear",
    ord("L"): "Millisecond",
    ord("m"): "MonthTwoDigit",
    ord("M"): "Minute",
    ord("S"): "Second",
    ord("u"): "DayOfWeekIso",
    ord("V"): "WeekOfYearIso",
    ord("Y"): "YearFourDigit",
    ord("z"): "TimezoneOffset",
    ord("Z"): "TimezoneOffsetMinutes",
}


def _lookup_format(c: int) -> str:
    return _FORMAT_MAP.get(c, "Literal")


def validate_format(fmt: str) -> tuple[int, str] | None:
    """mongod's ``checkFormatString<true>``: every ``%`` is followed by ``%``
    or a specifier in the map. Returns mongod's (code, message) for the first
    violation, or ``None``."""
    it = iter(fmt)
    for c in it:
        if c != "%":
            continue
        spec = next(it, None)
        if spec is None:
            return 18535, "Unmatched '%' at end of format string"
        valid = spec == "%" or (spec.isascii() and _lookup_format(ord(spec)) != "Literal")
        if not valid:
            return 18536, f"Invalid format character '%{spec}' in format string"
    return None


def _c_strtol(t: Tok, start: int) -> int:
    """C's ``strtol(s, NULL, 10)``: leading whitespace, an optional sign, then
    digits, saturating at the ``long`` range."""
    i = start
    while t.at(i) in (0x20, 0x09, 0x0A, 0x0B, 0x0C, 0x0D):
        i += 1
    negative = t.at(i) == 0x2D
    if t.at(i) in (0x2B, 0x2D):
        i += 1
    v = 0
    while 0x30 <= t.at(i) <= 0x39:
        v = v * 10 + (t.at(i) - 0x30)
        i += 1
    v = -v if negative else v
    return max(I64_MIN, min(I64_MAX, v))


def _parse_tz_minutes(t: Tok, time: Time) -> int:
    """``timelib_parse_tz_minutes``: a ``+``/``-`` and a number of minutes, as
    seconds."""
    begin = t.p
    if t.cur() not in (0x2B, 0x2D):
        return UNSET
    t.p += 1
    while 0x30 <= t.cur() <= 0x39:
        t.p += 1
    time.zone_type = ZONETYPE_OFFSET
    time.dst = 0
    minutes = wrap_i64(_c_strtol(t, begin + 1) * 60)
    return minutes if t.at(begin) == 0x2B else wrap_i64(-minutes)


def parse_from_format(fmt: bytes, string: bytes) -> Parsed:  # noqa: C901, PLR0912, PLR0915
    """``timelib_parse_from_format_with_map(format, string, ...)`` with
    mongod's map and prefix."""
    f = Tok(fmt)
    fp = 0
    t = Tok(string)
    time = Time()
    errors: list[Message] = []
    warnings: list[Message] = []
    prefix_found = False
    iso_year = UNSET
    iso_week_of_year = UNSET
    iso_day_of_week = UNSET

    def error(code: int, text: str, at: int) -> None:
        errors.append(Message(code, at, t.at(at), text))

    while f.at(fp) != 0 and t.cur() != 0:
        begin = t.p
        fc = f.at(fp)
        # The prefix: a character outside a `%` sequence, or the second `%` of
        # `%%`, must match the input literally.
        if (not prefix_found and fc != 0x25) or (prefix_found and fc == 0x25):
            if fc != t.cur():
                error(ERR_FORMAT_LITERAL_MISMATCH, "Format literal not found", begin)
            t.p += 1
            fp += 1
            prefix_found = False
            continue
        if fc == 0x25:
            fp += 1
            prefix_found = True
            continue
        prefix_found = False

        def check_number(begin: int = begin) -> None:
            # TIMELIB_CHECK_NUMBER: strchr also matches the NUL, which the
            # loop condition has already ruled out.
            if not 0x30 <= t.cur() <= 0x39:
                error(ERR_UNEXPECTED_DATA, "Unexpected data found.", begin)

        spec = _lookup_format(fc)
        if spec == "DayTwoDigit":
            check_number()
            time.d = get_nr(t, 2)
            if time.d == UNSET:
                error(ERR_NO_TWO_DIGIT_DAY, "A two digit day could not be found", begin)
            else:
                time.have_date = True
        elif spec == "DayOfYear":
            check_number()
            if time.y == UNSET:
                error(
                    ERR_MERIDIAN_BEFORE_HOUR,
                    "A 'day of year' can only come after a year has been found",
                    begin,
                )
            tmp = get_nr(t, 3)
            if tmp == UNSET:
                error(
                    ERR_NO_THREE_DIGIT_DAY_OF_YEAR,
                    "A three digit day-of-year could not be found",
                    begin,
                )
            elif time.y != UNSET:
                time.have_date = True
                time.m = 1
                time.d = tmp + 1
                do_normalize(time)
        elif spec == "MonthTwoDigit":
            check_number()
            time.m = get_nr(t, 2)
            if time.m == UNSET:
                error(ERR_NO_TWO_DIGIT_MONTH, "A two digit month could not be found", begin)
            else:
                time.have_date = True
        elif spec == "TextualMonth":
            tmp = lookup_month(t)
            if tmp == 0:
                error(ERR_NO_TEXTUAL_MONTH, "A textual month could not be found", begin)
            else:
                time.have_date = True
                time.m = tmp
        elif spec == "YearFourDigit":
            check_number()
            time.y = get_nr(t, 4)
            if time.y == UNSET:
                error(ERR_NO_FOUR_DIGIT_YEAR, "A four digit year could not be found", begin)
            else:
                time.have_date = True
        elif spec == "Hour24":
            check_number()
            time.h = get_nr(t, 2)
            if time.h == UNSET:
                error(ERR_NO_TWO_DIGIT_HOUR, "A two digit hour could not be found", begin)
            else:
                time.have_time = 1
        elif spec == "Minute":
            check_number()
            minute, length = get_nr_ex(t, 2)
            if minute == UNSET or length != 2:
                error(ERR_NO_TWO_DIGIT_MINUTE, "A two digit minute could not be found", begin)
            else:
                time.have_time = 1
                time.i = minute
        elif spec == "Second":
            check_number()
            sec, length = get_nr_ex(t, 2)
            if sec == UNSET or length != 2:
                error(ERR_NO_TWO_DIGIT_SECOND, "A two digit second could not be found", begin)
            else:
                time.have_time = 1
                time.s = sec
        elif spec == "Millisecond":
            check_number()
            tptr = t.p
            frac = get_nr(t, 3)
            scanned = t.p - tptr
            if frac == UNSET or scanned < 1:
                error(
                    ERR_NO_THREE_DIGIT_MILLISECOND,
                    "A three digit millisecond could not be found",
                    begin,
                )
            else:
                # (f * pow(10, 3 - (ptr - tptr)) * 1000), truncated.
                time.us = int(frac * 10.0 ** (3 - scanned) * 1000.0)
        elif spec == "YearIso":
            iso_year = get_nr(t, 4)
            if iso_year == UNSET:
                error(ERR_NO_FOUR_DIGIT_YEAR_ISO, "A four digit ISO year could not be found", begin)
            else:
                time.have_date = True
        elif spec == "WeekOfYearIso":
            iso_week_of_year = get_nr(t, 2)
            if iso_week_of_year == UNSET:
                error(ERR_NO_TWO_DIGIT_WEEK, "A two digit ISO week could not be found", begin)
            elif not 1 <= iso_week_of_year <= 53:
                error(ERR_INVALID_WEEK, "ISO Week must be between 1 and 53", begin)
            else:
                time.have_date = True
        elif spec == "DayOfWeekIso":
            iso_day_of_week = get_nr(t, 1)
            if iso_day_of_week == UNSET:
                error(ERR_NO_DAY_OF_WEEK, "A single digit day of week could not be found", begin)
            elif not 1 <= iso_day_of_week <= 7:
                error(ERR_INVALID_DAY_OF_WEEK, "Day of week must be between 1 and 7", begin)
            else:
                time.have_date = True
        elif spec == "TimezoneOffset":
            z, not_found = parse_zone(t, time)
            time.z = z
            if not_found:
                error(
                    ERR_TZID_NOT_FOUND,
                    "The timezone could not be found in the database",
                    begin,
                )
            else:
                time.have_zone = 1
        elif spec == "TimezoneOffsetMinutes":
            time.z = _parse_tz_minutes(t, time)
            if time.z == UNSET:
                error(ERR_INVALID_TZ_OFFSET, "Invalid timezone offset in minutes", begin)
            else:
                time.have_zone = 1
        else:  # Literal
            if fc != t.cur():
                error(ERR_WRONG_FORMAT_SEP, "The format separator does not match", begin)
            t.p += 1
        fp += 1

    if t.cur() != 0:
        error(ERR_TRAILING_DATA, "Trailing data", t.p)
    # mongod's map has no reset or allow-extra specifiers, so any format left
    # over is data missing.
    if f.at(fp) != 0:
        error(ERR_DATA_MISSING, "Not enough data available to satisfy format", t.p)

    # Clean up a bit.
    if any(v != UNSET for v in (time.h, time.i, time.s, time.us)):
        if time.h == UNSET:
            time.h = 0
        if time.i == UNSET:
            time.i = 0
        if time.s == UNSET:
            time.s = 0
        if time.us == UNSET:
            time.us = 0

    # Mixing ISO dates with natural dates.
    if time.y != UNSET and (
        iso_week_of_year != UNSET or iso_year != UNSET or iso_day_of_week != UNSET
    ):
        error(
            ERR_MIX_ISO_WITH_NATURAL,
            "Mixing of ISO dates with natural dates is not allowed",
            t.p,
        )
    if iso_year != UNSET and (time.y != UNSET or time.m != UNSET or time.d != UNSET):
        error(
            ERR_MIX_ISO_WITH_NATURAL,
            "Mixing of ISO dates with natural dates is not allowed",
            t.p,
        )

    if iso_year != UNSET:
        iw = 1 if iso_week_of_year == UNSET else iso_week_of_year
        id_ = 1 if iso_day_of_week == UNSET else iso_day_of_week
        time.y, time.m, time.d = date_from_isodate(iso_year, iw, id_)
    elif iso_week_of_year != UNSET or iso_day_of_week != UNSET:
        warnings.append(Message(WARN_INVALID_DATE, t.p, t.at(t.p), "The parsed date was invalid"))

    if UNSET not in (time.h, time.i, time.s) and not valid_time(time.h, time.i, time.s):
        warnings.append(Message(WARN_INVALID_TIME, t.p, t.at(t.p), "The parsed time was invalid"))
    if UNSET not in (time.y, time.m, time.d) and not valid_date(time.y, time.m, time.d):
        warnings.append(Message(WARN_INVALID_DATE, t.p, t.at(t.p), "The parsed date was invalid"))

    return Parsed(time, errors, warnings)

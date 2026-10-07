"""``timelib_update_ts`` and the calendar helpers it needs, ported from timelib
2022.13 ``tm2unixtime.c`` and ``dow.c`` (MIT; see ``__init__.py``).

mongod calls ``timelib_update_ts(t, nullptr)``: no zone database, so the
``TIMELIB_ZONETYPE_ID`` / default branch of ``do_adjust_timezone`` is a no-op
and only an offset or an abbreviation moves the result.

C integer division TRUNCATES toward zero and ``%`` takes the dividend's sign;
Python's ``//`` and ``%`` floor. Every division here goes through
:func:`cdiv` / :func:`cmod` so negative operands answer what timelib answers.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from secantus.timelib.consts import (
    SPECIAL_DAY_OF_WEEK_IN_MONTH,
    SPECIAL_FIRST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH,
    SPECIAL_WEEKDAY,
    UNSET,
    ZONETYPE_ABBR,
    ZONETYPE_OFFSET,
)

if TYPE_CHECKING:
    from secantus.timelib import Time

DAYS_PER_ERA = 146_097
YEARS_PER_ERA = 400
DAYS_PER_YEAR = 365
HINNANT_EPOCH_SHIFT = 719_468
SECS_PER_DAY = 86_400
SECS_PER_HOUR = 3_600

#                    dec  jan  feb  mar  apr  may  jun  jul  aug  sep  oct  nov  dec
DAYS_IN_MONTH_LEAP = (31, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
DAYS_IN_MONTH = (31, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)


def cdiv(a: int, b: int) -> int:
    """C's ``a / b``: truncating toward zero."""
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


def cmod(a: int, b: int) -> int:
    """C's ``a % b``: the sign of the dividend."""
    return a - b * cdiv(a, b)


def is_leap(y: int) -> bool:
    return cmod(y, 4) == 0 and (cmod(y, 100) != 0 or cmod(y, 400) == 0)


def days_in_month(y: int, m: int) -> int:
    """``timelib_days_in_month`` (``dow.c``), for a month already in 1..12."""
    return (DAYS_IN_MONTH_LEAP if is_leap(y) else DAYS_IN_MONTH)[m]


def positive_mod(x: int, y: int) -> int:
    tmp = cmod(x, y)
    return tmp + y if tmp < 0 else tmp


_M_TABLE_COMMON = (-1, 0, 3, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5)
_M_TABLE_LEAP = (-1, 6, 2, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5)


def day_of_week(y: int, m: int, d: int) -> int:
    """``timelib_day_of_week`` (``dow.c``), Sunday = 0."""
    c1 = 6 - positive_mod(cdiv(positive_mod(y, 400), 100), 4) * 2
    y1 = positive_mod(y, 100)
    m1 = (_M_TABLE_LEAP if is_leap(y) else _M_TABLE_COMMON)[m]
    return positive_mod(c1 + y1 + m1 + cdiv(y1, 4) + d, 7)


def daynr_from_weeknr(iy: int, iw: int, id_: int) -> int:
    """``timelib_daynr_from_weeknr`` (``dow.c``)."""
    dow = day_of_week(iy, 1, 1)
    day = -(dow - 7 if dow > 4 else dow)
    return day + ((iw - 1) * 7) + id_


def do_range_limit(start: int, end: int, adj: int, a: int, b: int) -> tuple[int, int]:
    """``do_range_limit``: returns the new ``(a, b)``."""
    if a < start:
        a_plus_1 = a + 1
        b -= cdiv(start - a_plus_1, adj) + 1
        a += adj * cdiv(start - a_plus_1, adj)
        a += adj
    if a >= end:
        b += cdiv(a, adj)
        a -= adj * cdiv(a, adj)
    return a, b


def do_range_limit_days(t: Time) -> bool:
    """``do_range_limit_days`` over ``t.y`` / ``t.m`` / ``t.d``."""
    retval = False
    y, m, d = t.y, t.m, t.d
    if d >= DAYS_PER_ERA or d <= -DAYS_PER_ERA:
        y += YEARS_PER_ERA * cdiv(d, DAYS_PER_ERA)
        d -= DAYS_PER_ERA * cdiv(d, DAYS_PER_ERA)
    m, y = do_range_limit(1, 13, 12, m, y)
    current = DAYS_IN_MONTH_LEAP if is_leap(y) else DAYS_IN_MONTH
    while d <= 0 and m > 0:
        previous_month, previous_year = m - 1, y
        if previous_month < 1:
            previous_month += 12
            previous_year -= 1
        d += (DAYS_IN_MONTH_LEAP if is_leap(previous_year) else DAYS_IN_MONTH)[previous_month]
        m -= 1
        retval = True
    while d > 0 and m <= 12 and d > current[m]:
        d -= current[m]
        m += 1
        retval = True
    t.y, t.m, t.d = y, m, d
    return retval


def magic_date_calc(t: Time) -> None:
    """``magic_date_calc``: the epoch short cut of ``timelib_do_normalize``."""
    if t.d < -719_498:
        return
    g = t.d + HINNANT_EPOCH_SHIFT - 1
    y = cdiv(10_000 * g + 14_780, 3_652_425)
    ddd = g - ((365 * y) + cdiv(y, 4) - cdiv(y, 100) + cdiv(y, 400))
    if ddd < 0:
        y -= 1
        ddd = g - ((365 * y) + cdiv(y, 4) - cdiv(y, 100) + cdiv(y, 400))
    mi = cdiv(100 * ddd + 52, 3060)
    mm = cmod(mi + 2, 12) + 1
    y += cdiv(mi + 2, 12)
    dd = ddd - cdiv(mi * 306 + 5, 10) + 1
    t.y, t.m, t.d = y, mm, dd


def do_normalize(t: Time) -> None:
    """``timelib_do_normalize``."""
    if t.us != UNSET:
        t.us, t.s = do_range_limit(0, 1_000_000, 1_000_000, t.us, t.s)
    if t.s != UNSET:
        t.s, t.i = do_range_limit(0, 60, 60, t.s, t.i)
    if t.s != UNSET:
        t.i, t.h = do_range_limit(0, 60, 60, t.i, t.h)
    if t.s != UNSET:
        t.h, t.d = do_range_limit(0, 24, 24, t.h, t.d)
    t.m, t.y = do_range_limit(1, 13, 12, t.m, t.y)
    if t.y == 1970 and t.m == 1 and t.d != 1:
        magic_date_calc(t)
    while do_range_limit_days(t):
        pass
    t.m, t.y = do_range_limit(1, 13, 12, t.m, t.y)


def do_adjust_for_weekday(t: Time) -> None:
    current_dow = day_of_week(t.y, t.m, t.d)
    r = t.relative
    if r.weekday_behavior == 2:
        if current_dow == 0 and r.weekday != 0:
            r.weekday -= 7
        if r.weekday == 0 and current_dow != 0:
            r.weekday = 7
        t.d -= current_dow
        t.d += r.weekday
        return
    difference = r.weekday - current_dow
    if (r.d < 0 and difference < 0) or (r.d >= 0 and difference <= -r.weekday_behavior):
        difference += 7
    if r.weekday >= 0:
        t.d += difference
    else:
        t.d -= 7 - (abs(r.weekday) - current_dow)
    r.have_weekday_relative = False


def do_adjust_relative(t: Time) -> None:
    if t.relative.have_weekday_relative:
        do_adjust_for_weekday(t)
    do_normalize(t)
    if t.have_relative:
        r = t.relative
        t.us += r.us
        t.s += r.s
        t.i += r.i
        t.h += r.h
        t.d += r.d
        t.m += r.m
        t.y += r.y
    if t.relative.first_last_day_of == SPECIAL_FIRST_DAY_OF_MONTH:
        t.d = 1
    elif t.relative.first_last_day_of == SPECIAL_LAST_DAY_OF_MONTH:
        t.d = 0
        t.m += 1
    do_normalize(t)


def do_adjust_special_weekday(t: Time) -> None:
    count = t.relative.special_amount
    dow = day_of_week(t.y, t.m, t.d)
    t.d += cdiv(count, 5) * 7
    rem = cmod(count, 5)
    if count > 0:
        if rem == 0:
            if dow == 0:
                t.d -= 2
            elif dow == 6:
                t.d -= 1
        elif dow == 6:
            t.d += 1
        elif dow + rem > 5:
            t.d += 2
    elif rem == 0:
        if dow == 6:
            t.d += 2
        elif dow == 0:
            t.d += 1
    elif dow == 0:
        t.d -= 1
    elif dow + rem < 1:
        t.d -= 2
    t.d += rem


def do_adjust_special(t: Time) -> None:
    if t.relative.have_special_relative and t.relative.special_type == SPECIAL_WEEKDAY:
        do_adjust_special_weekday(t)
    do_normalize(t)
    t.relative.special_type = 0
    t.relative.special_amount = 0


def do_adjust_special_early(t: Time) -> None:
    r = t.relative
    if r.have_special_relative:
        if r.special_type == SPECIAL_DAY_OF_WEEK_IN_MONTH:
            t.d = 1
            t.m += r.m
            r.m = 0
        elif r.special_type == SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH:
            t.d = 1
            t.m += r.m + 1
            r.m = 0
    if r.first_last_day_of == SPECIAL_FIRST_DAY_OF_MONTH:
        t.d = 1
    elif r.first_last_day_of == SPECIAL_LAST_DAY_OF_MONTH:
        t.d = 0
        t.m += 1
    do_normalize(t)


def epoch_days_from_time(t: Time) -> int:
    """``timelib_epoch_days_from_time``."""
    y = t.y - (1 if t.m <= 2 else 0)
    era = cdiv(y if y >= 0 else y - 399, YEARS_PER_ERA)
    year_of_era = y - era * YEARS_PER_ERA
    day_of_year = cdiv(153 * (t.m + (-3 if t.m > 2 else 9)) + 2, 5) + t.d - 1
    day_of_era = (
        year_of_era * DAYS_PER_YEAR + cdiv(year_of_era, 4) - cdiv(year_of_era, 100) + day_of_year
    )
    return era * DAYS_PER_ERA + day_of_era - HINNANT_EPOCH_SHIFT


def update_ts(t: Time) -> None:
    """``timelib_update_ts(t, nullptr)``: resolve the relative parts and the
    zone into ``t.sse``, seconds since the epoch."""
    do_adjust_special_early(t)
    do_adjust_relative(t)
    do_adjust_special(t)
    t.sse = t.h * SECS_PER_HOUR + t.i * 60 + t.s
    t.sse += epoch_days_from_time(t) * (SECS_PER_DAY // 2)
    t.sse += epoch_days_from_time(t) * (SECS_PER_DAY // 2)
    # do_adjust_timezone, with no zone database.
    if t.zone_type == ZONETYPE_OFFSET:
        t.sse += -t.z
    elif t.zone_type == ZONETYPE_ABBR:
        t.sse += -t.z - t.dst * SECS_PER_HOUR
    t.have_relative = False
    t.relative.have_weekday_relative = False
    t.relative.have_special_relative = False
    t.relative.first_last_day_of = 0


def date_from_isodate(iy: int, iw: int, id_: int) -> tuple[int, int, int]:
    """``timelib_date_from_isodate`` (``dow.c``): the calendar date of an ISO
    year, week and day of week."""
    daynr = daynr_from_weeknr(iy, iw, id_) + 1
    y = iy
    leap = is_leap(y)
    while daynr <= 0:
        y -= 1
        leap = is_leap(y)
        daynr += 366 if leap else 365
    while daynr > (366 if leap else 365):
        daynr -= 366 if leap else 365
        y += 1
        leap = is_leap(y)
    table = DAYS_IN_MONTH_LEAP if leap else DAYS_IN_MONTH
    m = 1
    while daynr > table[m]:
        daynr -= table[m]
        m += 1
    return y, m, daynr

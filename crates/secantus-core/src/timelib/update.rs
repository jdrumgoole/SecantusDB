//! `timelib_update_ts` and the calendar helpers it needs, ported from timelib
//! 2022.13 `tm2unixtime.c` and `dow.c` (MIT; see `mod.rs`).
//!
//! mongod calls `timelib_update_ts(t, nullptr)`: no zone database, so the
//! `TIMELIB_ZONETYPE_ID` / default branch of `do_adjust_timezone` is a no-op
//! and only an offset or an abbreviation moves the result.

use super::{
    Time, SPECIAL_DAY_OF_WEEK_IN_MONTH, SPECIAL_FIRST_DAY_OF_MONTH, SPECIAL_LAST_DAY_OF_MONTH,
    SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH, SPECIAL_WEEKDAY, UNSET, ZONETYPE_ABBR, ZONETYPE_OFFSET,
};

const DAYS_PER_ERA: i64 = 146_097;
const YEARS_PER_ERA: i64 = 400;
const DAYS_PER_YEAR: i64 = 365;
const HINNANT_EPOCH_SHIFT: i64 = 719_468;
const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3_600;

//                                dec  jan  feb  mar  apr  may  jun  jul  aug  sep  oct  nov  dec
const DAYS_IN_MONTH_LEAP: [i64; 13] = [31, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const DAYS_IN_MONTH: [i64; 13] = [31, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// `timelib_days_in_month` (`dow.c`), for a month already in 1..=12.
pub(crate) fn days_in_month(y: i64, m: i64) -> i64 {
    let table = if is_leap(y) {
        &DAYS_IN_MONTH_LEAP
    } else {
        &DAYS_IN_MONTH
    };
    table[m as usize]
}

fn positive_mod(x: i64, y: i64) -> i64 {
    let tmp = x % y;
    if tmp < 0 {
        tmp + y
    } else {
        tmp
    }
}

/// `timelib_day_of_week` (`dow.c`), Sunday = 0.
pub(crate) fn day_of_week(y: i64, m: i64, d: i64) -> i64 {
    const M_TABLE_COMMON: [i64; 13] = [-1, 0, 3, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5];
    const M_TABLE_LEAP: [i64; 13] = [-1, 6, 2, 3, 6, 1, 4, 6, 2, 5, 0, 3, 5];
    let c1 = 6 - positive_mod(positive_mod(y, 400) / 100, 4) * 2;
    let y1 = positive_mod(y, 100);
    let m1 = if is_leap(y) {
        M_TABLE_LEAP[m as usize]
    } else {
        M_TABLE_COMMON[m as usize]
    };
    positive_mod(c1 + y1 + m1 + (y1 / 4) + d, 7)
}

/// `timelib_daynr_from_weeknr` (`dow.c`).
pub(crate) fn daynr_from_weeknr(iy: i64, iw: i64, id: i64) -> i64 {
    let dow = day_of_week(iy, 1, 1);
    let day = -(if dow > 4 { dow - 7 } else { dow });
    day + ((iw - 1) * 7) + id
}

/// `do_range_limit`.
fn do_range_limit(start: i64, end: i64, adj: i64, a: &mut i64, b: &mut i64) {
    if *a < start {
        let a_plus_1 = *a + 1;
        *b -= (start - a_plus_1) / adj + 1;
        *a += adj * ((start - a_plus_1) / adj);
        *a += adj;
    }
    if *a >= end {
        *b += *a / adj;
        *a -= adj * (*a / adj);
    }
}

/// `do_range_limit_days`.
fn do_range_limit_days(y: &mut i64, m: &mut i64, d: &mut i64) -> bool {
    let mut retval = false;
    if *d >= DAYS_PER_ERA || *d <= -DAYS_PER_ERA {
        *y += YEARS_PER_ERA * (*d / DAYS_PER_ERA);
        *d -= DAYS_PER_ERA * (*d / DAYS_PER_ERA);
    }
    do_range_limit(1, 13, 12, m, y);
    let current = if is_leap(*y) {
        DAYS_IN_MONTH_LEAP
    } else {
        DAYS_IN_MONTH
    };
    while *d <= 0 && *m > 0 {
        let (mut previous_month, previous_year) = (*m - 1, *y);
        let previous_year = if previous_month < 1 {
            previous_month += 12;
            previous_year - 1
        } else {
            previous_year
        };
        *d += if is_leap(previous_year) {
            DAYS_IN_MONTH_LEAP[previous_month as usize]
        } else {
            DAYS_IN_MONTH[previous_month as usize]
        };
        *m -= 1;
        retval = true;
    }
    while *d > 0 && *m <= 12 && *d > current[*m as usize] {
        *d -= current[*m as usize];
        *m += 1;
        retval = true;
    }
    retval
}

/// `magic_date_calc`: the epoch short cut of `timelib_do_normalize`.
fn magic_date_calc(t: &mut Time) {
    if t.d < -719_498 {
        return;
    }
    let g = t.d + HINNANT_EPOCH_SHIFT - 1;
    let mut y = (10_000 * g + 14_780) / 3_652_425;
    let mut ddd = g - ((365 * y) + (y / 4) - (y / 100) + (y / 400));
    if ddd < 0 {
        y -= 1;
        ddd = g - ((365 * y) + (y / 4) - (y / 100) + (y / 400));
    }
    let mi = (100 * ddd + 52) / 3060;
    let mm = ((mi + 2) % 12) + 1;
    y += (mi + 2) / 12;
    let dd = ddd - ((mi * 306 + 5) / 10) + 1;
    t.y = y;
    t.m = mm;
    t.d = dd;
}

/// `timelib_do_normalize`.
pub(super) fn do_normalize(t: &mut Time) {
    if t.us != UNSET {
        do_range_limit(0, 1_000_000, 1_000_000, &mut t.us, &mut t.s);
    }
    if t.s != UNSET {
        do_range_limit(0, 60, 60, &mut t.s, &mut t.i);
    }
    if t.s != UNSET {
        do_range_limit(0, 60, 60, &mut t.i, &mut t.h);
    }
    if t.s != UNSET {
        do_range_limit(0, 24, 24, &mut t.h, &mut t.d);
    }
    do_range_limit(1, 13, 12, &mut t.m, &mut t.y);
    if t.y == 1970 && t.m == 1 && t.d != 1 {
        magic_date_calc(t);
    }
    while do_range_limit_days(&mut t.y, &mut t.m, &mut t.d) {}
    do_range_limit(1, 13, 12, &mut t.m, &mut t.y);
}

/// `do_adjust_for_weekday`.
fn do_adjust_for_weekday(t: &mut Time) {
    let current_dow = day_of_week(t.y, t.m, t.d);
    let r = &mut t.relative;
    if r.weekday_behavior == 2 {
        if current_dow == 0 && r.weekday != 0 {
            r.weekday -= 7;
        }
        if r.weekday == 0 && current_dow != 0 {
            r.weekday = 7;
        }
        t.d -= current_dow;
        t.d += t.relative.weekday;
        return;
    }
    let mut difference = r.weekday - current_dow;
    if (r.d < 0 && difference < 0) || (r.d >= 0 && difference <= -r.weekday_behavior) {
        difference += 7;
    }
    if r.weekday >= 0 {
        t.d += difference;
    } else {
        t.d -= 7 - (r.weekday.abs() - current_dow);
    }
    t.relative.have_weekday_relative = false;
}

/// `do_adjust_relative`.
fn do_adjust_relative(t: &mut Time) {
    if t.relative.have_weekday_relative {
        do_adjust_for_weekday(t);
    }
    do_normalize(t);
    if t.have_relative {
        t.us += t.relative.us;
        t.s += t.relative.s;
        t.i += t.relative.i;
        t.h += t.relative.h;
        t.d += t.relative.d;
        t.m += t.relative.m;
        t.y += t.relative.y;
    }
    match t.relative.first_last_day_of {
        SPECIAL_FIRST_DAY_OF_MONTH => t.d = 1,
        SPECIAL_LAST_DAY_OF_MONTH => {
            t.d = 0;
            t.m += 1;
        }
        _ => {}
    }
    do_normalize(t);
}

/// `do_adjust_special_weekday`.
fn do_adjust_special_weekday(t: &mut Time) {
    let count = t.relative.special_amount;
    let dow = day_of_week(t.y, t.m, t.d);
    t.d += (count / 5) * 7;
    let rem = count % 5;
    if count > 0 {
        if rem == 0 {
            if dow == 0 {
                t.d -= 2;
            } else if dow == 6 {
                t.d -= 1;
            }
        } else if dow == 6 {
            t.d += 1;
        } else if dow + rem > 5 {
            t.d += 2;
        }
    } else if rem == 0 {
        if dow == 6 {
            t.d += 2;
        } else if dow == 0 {
            t.d += 1;
        }
    } else if dow == 0 {
        t.d -= 1;
    } else if dow + rem < 1 {
        t.d -= 2;
    }
    t.d += rem;
}

/// `do_adjust_special`.
fn do_adjust_special(t: &mut Time) {
    if t.relative.have_special_relative && t.relative.special_type == SPECIAL_WEEKDAY {
        do_adjust_special_weekday(t);
    }
    do_normalize(t);
    t.relative.special_type = 0;
    t.relative.special_amount = 0;
}

/// `do_adjust_special_early`.
fn do_adjust_special_early(t: &mut Time) {
    if t.relative.have_special_relative {
        match t.relative.special_type {
            SPECIAL_DAY_OF_WEEK_IN_MONTH => {
                t.d = 1;
                t.m += t.relative.m;
                t.relative.m = 0;
            }
            SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH => {
                t.d = 1;
                t.m += t.relative.m + 1;
                t.relative.m = 0;
            }
            _ => {}
        }
    }
    match t.relative.first_last_day_of {
        SPECIAL_FIRST_DAY_OF_MONTH => t.d = 1,
        SPECIAL_LAST_DAY_OF_MONTH => {
            t.d = 0;
            t.m += 1;
        }
        _ => {}
    }
    do_normalize(t);
}

/// `timelib_epoch_days_from_time`.
fn epoch_days_from_time(t: &Time) -> i64 {
    let y = t.y - i64::from(t.m <= 2);
    let era = (if y >= 0 { y } else { y - 399 }) / YEARS_PER_ERA;
    let year_of_era = y - era * YEARS_PER_ERA;
    let day_of_year = (153 * (t.m + if t.m > 2 { -3 } else { 9 }) + 2) / 5 + t.d - 1;
    let day_of_era =
        year_of_era * DAYS_PER_YEAR + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_ERA + day_of_era - HINNANT_EPOCH_SHIFT
}

/// `timelib_update_ts(t, nullptr)`: resolve relative parts and the zone into
/// `t.sse`, seconds since the epoch.
pub(crate) fn update_ts(t: &mut Time) {
    do_adjust_special_early(t);
    do_adjust_relative(t);
    do_adjust_special(t);
    t.sse = t.h * SECS_PER_HOUR + t.i * 60 + t.s;
    t.sse += epoch_days_from_time(t) * (SECS_PER_DAY / 2);
    t.sse += epoch_days_from_time(t) * (SECS_PER_DAY / 2);
    // do_adjust_timezone, with no zone database.
    match t.zone_type {
        ZONETYPE_OFFSET => t.sse += -t.z,
        ZONETYPE_ABBR => t.sse += -t.z - t.dst * SECS_PER_HOUR,
        _ => {}
    }
    t.have_relative = false;
    t.relative.have_weekday_relative = false;
    t.relative.have_special_relative = false;
    t.relative.first_last_day_of = 0;
}

/// `timelib_date_from_isodate` (`dow.c`): the calendar date of an ISO year,
/// week and day of week.
pub(super) fn date_from_isodate(iy: i64, iw: i64, id: i64) -> (i64, i64, i64) {
    let mut daynr = daynr_from_weeknr(iy, iw, id) + 1;
    let mut y = iy;
    let mut leap = is_leap(y);
    while daynr <= 0 {
        y -= 1;
        leap = is_leap(y);
        daynr += if leap { 366 } else { 365 };
    }
    while daynr > if leap { 366 } else { 365 } {
        daynr -= if leap { 366 } else { 365 };
        y += 1;
        leap = is_leap(y);
    }
    // ml_table_{leap,common}: index 0 unused.
    let table = if leap {
        &DAYS_IN_MONTH_LEAP
    } else {
        &DAYS_IN_MONTH
    };
    let mut m = 1;
    while daynr > table[m as usize] {
        daynr -= table[m as usize];
        m += 1;
    }
    (y, m, daynr)
}

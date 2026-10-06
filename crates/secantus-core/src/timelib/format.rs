//! `timelib_parse_from_format_with_map` (timelib 2022.13 `parse_date.re`,
//! MIT; see `mod.rs`), with the configuration mongod passes it: the
//! `kDateFromStringFormatMap` of `date_time_support.cpp` and `%` as the
//! required prefix. This is `$dateFromString` with a `format`.
//!
//! Like the free-form port, it is literal: the loop keeps going after an
//! error, and every error and warning carries the position and character
//! timelib would report, because mongod prints all of them.

use super::scan::{get_nr, get_nr_ex, lookup_month, parse_zone, Tok};
use super::scan::{valid_date, valid_time};
use super::update::{date_from_isodate, do_normalize};
use super::{Message, Parsed, Time, ERR_TZID_NOT_FOUND, UNSET, ZONETYPE_OFFSET};

const ERR_UNEXPECTED_DATA: i32 = 0x207;
const ERR_NO_TWO_DIGIT_DAY: i32 = 0x209;
const ERR_NO_THREE_DIGIT_DAY_OF_YEAR: i32 = 0x20a;
const ERR_NO_TWO_DIGIT_MONTH: i32 = 0x20b;
const ERR_NO_TEXTUAL_MONTH: i32 = 0x20c;
const ERR_NO_FOUR_DIGIT_YEAR: i32 = 0x20e;
const ERR_NO_TWO_DIGIT_HOUR: i32 = 0x20f;
const ERR_MERIDIAN_BEFORE_HOUR: i32 = 0x211;
const ERR_NO_TWO_DIGIT_MINUTE: i32 = 0x213;
const ERR_NO_TWO_DIGIT_SECOND: i32 = 0x214;
const ERR_NO_THREE_DIGIT_MILLISECOND: i32 = 0x21c;
const ERR_TRAILING_DATA: i32 = 0x21a;
const ERR_DATA_MISSING: i32 = 0x21b;
const ERR_FORMAT_LITERAL_MISMATCH: i32 = 0x224;
const ERR_MIX_ISO_WITH_NATURAL: i32 = 0x225;
const ERR_WRONG_FORMAT_SEP: i32 = 0x219;
const ERR_NO_FOUR_DIGIT_YEAR_ISO: i32 = 0x21d;
const ERR_NO_TWO_DIGIT_WEEK: i32 = 0x21e;
const ERR_INVALID_WEEK: i32 = 0x21f;
const ERR_NO_DAY_OF_WEEK: i32 = 0x220;
const ERR_INVALID_DAY_OF_WEEK: i32 = 0x221;
const ERR_INVALID_TZ_OFFSET: i32 = 0x223;
const WARN_INVALID_TIME: i32 = 0x102;
const WARN_INVALID_DATE: i32 = 0x103;

/// The `timelib_format_specifier_code`s mongod's map uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spec {
    TextualMonth,
    DayTwoDigit,
    YearIso,
    Hour24,
    DayOfYear,
    Millisecond,
    MonthTwoDigit,
    Minute,
    Second,
    DayOfWeekIso,
    WeekOfYearIso,
    YearFourDigit,
    TimezoneOffset,
    TimezoneOffsetMinutes,
    Literal,
}

/// `timelib_lookup_format` over `kDateFromStringFormatMap`.
fn lookup_format(c: u8) -> Spec {
    match c {
        b'b' | b'B' => Spec::TextualMonth,
        b'd' => Spec::DayTwoDigit,
        b'G' => Spec::YearIso,
        b'H' => Spec::Hour24,
        b'j' => Spec::DayOfYear,
        b'L' => Spec::Millisecond,
        b'm' => Spec::MonthTwoDigit,
        b'M' => Spec::Minute,
        b'S' => Spec::Second,
        b'u' => Spec::DayOfWeekIso,
        b'V' => Spec::WeekOfYearIso,
        b'Y' => Spec::YearFourDigit,
        b'z' => Spec::TimezoneOffset,
        b'Z' => Spec::TimezoneOffsetMinutes,
        _ => Spec::Literal,
    }
}

/// mongod's `checkFormatString<true>`: every `%` is followed by `%` or a
/// specifier in the map. Returns mongod's (code, message) for the first
/// violation.
pub(crate) fn validate_format(format: &str) -> Result<(), (i32, String)> {
    let mut it = format.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            continue;
        }
        let Some(spec) = it.next() else {
            return Err((18535, "Unmatched '%' at end of format string".into()));
        };
        let valid = spec == '%' || (spec.is_ascii() && lookup_format(spec as u8) != Spec::Literal);
        if !valid {
            return Err((
                18536,
                format!("Invalid format character '%{spec}' in format string"),
            ));
        }
    }
    Ok(())
}

fn message(code: i32, text: &'static str, string: &Tok, at: usize) -> Message {
    Message {
        code,
        position: at,
        character: string.at(at),
        message: text,
    }
}

/// C's `strtol(s, NULL, 10)`: leading whitespace, an optional sign, then
/// digits, saturating at the `long` range.
fn c_strtol(t: &Tok, from: usize) -> i64 {
    let mut i = from;
    while matches!(t.at(i), b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let negative = t.at(i) == b'-';
    if matches!(t.at(i), b'+' | b'-') {
        i += 1;
    }
    let mut v: i64 = 0;
    while t.at(i).is_ascii_digit() {
        let d = i64::from(t.at(i) - b'0');
        v = if negative {
            v.saturating_mul(10).saturating_sub(d)
        } else {
            v.saturating_mul(10).saturating_add(d)
        };
        i += 1;
    }
    v
}

/// `timelib_parse_tz_minutes`: a `+`/`-` and a number of minutes, as seconds.
fn parse_tz_minutes(t: &mut Tok, time: &mut Time) -> i64 {
    let begin = t.p;
    if !matches!(t.cur(), b'+' | b'-') {
        return UNSET;
    }
    t.p += 1;
    while t.cur().is_ascii_digit() {
        t.p += 1;
    }
    time.zone_type = ZONETYPE_OFFSET;
    time.dst = 0;
    let minutes = c_strtol(t, begin + 1).wrapping_mul(60);
    if t.at(begin) == b'+' {
        minutes
    } else {
        minutes.wrapping_neg()
    }
}

/// `timelib_parse_from_format_with_map(format, string, ...)` with mongod's
/// map and prefix.
pub(crate) fn parse_from_format(format: &[u8], string: &[u8]) -> Parsed {
    let f = Tok { b: format, p: 0 };
    let mut fp = 0usize;
    let mut t = Tok { b: string, p: 0 };
    let mut time = Time::unset();
    let mut errors: Vec<Message> = Vec::new();
    let mut warnings: Vec<Message> = Vec::new();
    let mut prefix_found = false;
    let mut iso_year = UNSET;
    let mut iso_week_of_year = UNSET;
    let mut iso_day_of_week = UNSET;

    macro_rules! error {
        ($code:expr, $text:expr, $at:expr) => {
            errors.push(message($code, $text, &t, $at))
        };
    }

    while f.at(fp) != 0 && t.cur() != 0 {
        let begin = t.p;
        let fc = f.at(fp);
        // The prefix: a character outside a `%` sequence, or the second `%`
        // of `%%`, must match the input literally.
        if (!prefix_found && fc != b'%') || (prefix_found && fc == b'%') {
            if fc != t.cur() {
                error!(
                    ERR_FORMAT_LITERAL_MISMATCH,
                    "Format literal not found", begin
                );
            }
            t.p += 1;
            fp += 1;
            prefix_found = false;
            continue;
        }
        if fc == b'%' {
            fp += 1;
            prefix_found = true;
            continue;
        }
        prefix_found = false;

        // TIMELIB_CHECK_NUMBER: strchr also matches the NUL, which the loop
        // condition has already ruled out.
        let check_number = |t: &Tok, errors: &mut Vec<Message>| {
            if !t.cur().is_ascii_digit() {
                errors.push(message(
                    ERR_UNEXPECTED_DATA,
                    "Unexpected data found.",
                    t,
                    begin,
                ));
            }
        };

        match lookup_format(fc) {
            Spec::DayTwoDigit => {
                check_number(&t, &mut errors);
                time.d = get_nr(&mut t, 2);
                if time.d == UNSET {
                    error!(
                        ERR_NO_TWO_DIGIT_DAY,
                        "A two digit day could not be found", begin
                    );
                } else {
                    time.have_date = true;
                }
            }
            Spec::DayOfYear => {
                check_number(&t, &mut errors);
                if time.y == UNSET {
                    error!(
                        ERR_MERIDIAN_BEFORE_HOUR,
                        "A 'day of year' can only come after a year has been found", begin
                    );
                }
                let tmp = get_nr(&mut t, 3);
                if tmp == UNSET {
                    error!(
                        ERR_NO_THREE_DIGIT_DAY_OF_YEAR,
                        "A three digit day-of-year could not be found", begin
                    );
                } else if time.y != UNSET {
                    time.have_date = true;
                    time.m = 1;
                    time.d = tmp + 1;
                    do_normalize(&mut time);
                }
            }
            Spec::MonthTwoDigit => {
                check_number(&t, &mut errors);
                time.m = get_nr(&mut t, 2);
                if time.m == UNSET {
                    error!(
                        ERR_NO_TWO_DIGIT_MONTH,
                        "A two digit month could not be found", begin
                    );
                } else {
                    time.have_date = true;
                }
            }
            Spec::TextualMonth => {
                let tmp = lookup_month(&mut t);
                if tmp == 0 {
                    error!(
                        ERR_NO_TEXTUAL_MONTH,
                        "A textual month could not be found", begin
                    );
                } else {
                    time.have_date = true;
                    time.m = tmp;
                }
            }
            Spec::YearFourDigit => {
                check_number(&t, &mut errors);
                time.y = get_nr(&mut t, 4);
                if time.y == UNSET {
                    error!(
                        ERR_NO_FOUR_DIGIT_YEAR,
                        "A four digit year could not be found", begin
                    );
                } else {
                    time.have_date = true;
                }
            }
            Spec::Hour24 => {
                check_number(&t, &mut errors);
                time.h = get_nr(&mut t, 2);
                if time.h == UNSET {
                    error!(
                        ERR_NO_TWO_DIGIT_HOUR,
                        "A two digit hour could not be found", begin
                    );
                } else {
                    time.have_time = 1;
                }
            }
            Spec::Minute => {
                check_number(&t, &mut errors);
                let (min, length) = get_nr_ex(&mut t, 2);
                if min == UNSET || length != 2 {
                    error!(
                        ERR_NO_TWO_DIGIT_MINUTE,
                        "A two digit minute could not be found", begin
                    );
                } else {
                    time.have_time = 1;
                    time.i = min;
                }
            }
            Spec::Second => {
                check_number(&t, &mut errors);
                let (sec, length) = get_nr_ex(&mut t, 2);
                if sec == UNSET || length != 2 {
                    error!(
                        ERR_NO_TWO_DIGIT_SECOND,
                        "A two digit second could not be found", begin
                    );
                } else {
                    time.have_time = 1;
                    time.s = sec;
                }
            }
            Spec::Millisecond => {
                check_number(&t, &mut errors);
                let tptr = t.p;
                let f = get_nr(&mut t, 3);
                let scanned = t.p - tptr;
                if f == UNSET || scanned < 1 {
                    error!(
                        ERR_NO_THREE_DIGIT_MILLISECOND,
                        "A three digit millisecond could not be found", begin
                    );
                } else {
                    // (f * pow(10, 3 - (ptr - tptr)) * 1000), truncated.
                    time.us = (f as f64 * 10f64.powi(3 - scanned as i32) * 1000.0) as i64;
                }
            }
            Spec::YearIso => {
                iso_year = get_nr(&mut t, 4);
                if iso_year == UNSET {
                    error!(
                        ERR_NO_FOUR_DIGIT_YEAR_ISO,
                        "A four digit ISO year could not be found", begin
                    );
                } else {
                    time.have_date = true;
                }
            }
            Spec::WeekOfYearIso => {
                iso_week_of_year = get_nr(&mut t, 2);
                if iso_week_of_year == UNSET {
                    error!(
                        ERR_NO_TWO_DIGIT_WEEK,
                        "A two digit ISO week could not be found", begin
                    );
                } else if !(1..=53).contains(&iso_week_of_year) {
                    error!(ERR_INVALID_WEEK, "ISO Week must be between 1 and 53", begin);
                } else {
                    time.have_date = true;
                }
            }
            Spec::DayOfWeekIso => {
                iso_day_of_week = get_nr(&mut t, 1);
                if iso_day_of_week == UNSET {
                    error!(
                        ERR_NO_DAY_OF_WEEK,
                        "A single digit day of week could not be found", begin
                    );
                } else if !(1..=7).contains(&iso_day_of_week) {
                    error!(
                        ERR_INVALID_DAY_OF_WEEK,
                        "Day of week must be between 1 and 7", begin
                    );
                } else {
                    time.have_date = true;
                }
            }
            Spec::TimezoneOffset => {
                let (z, not_found) = parse_zone(&mut t, &mut time);
                time.z = z;
                if not_found {
                    error!(
                        ERR_TZID_NOT_FOUND,
                        "The timezone could not be found in the database", begin
                    );
                } else {
                    time.have_zone = 1;
                }
            }
            Spec::TimezoneOffsetMinutes => {
                time.z = parse_tz_minutes(&mut t, &mut time);
                if time.z == UNSET {
                    error!(
                        ERR_INVALID_TZ_OFFSET,
                        "Invalid timezone offset in minutes", begin
                    );
                } else {
                    time.have_zone = 1;
                }
            }
            Spec::Literal => {
                if fc != t.cur() {
                    error!(
                        ERR_WRONG_FORMAT_SEP,
                        "The format separator does not match", begin
                    );
                }
                t.p += 1;
            }
        }
        fp += 1;
    }
    if t.cur() != 0 {
        error!(ERR_TRAILING_DATA, "Trailing data", t.p);
    }
    // mongod's map has no reset or allow-extra specifiers, so any format
    // left over is data missing.
    if f.at(fp) != 0 {
        error!(
            ERR_DATA_MISSING,
            "Not enough data available to satisfy format", t.p
        );
    }

    // Clean up a bit.
    if time.h != UNSET || time.i != UNSET || time.s != UNSET || time.us != UNSET {
        for v in [&mut time.h, &mut time.i, &mut time.s, &mut time.us] {
            if *v == UNSET {
                *v = 0;
            }
        }
    }

    // Mixing ISO dates with natural dates.
    if time.y != UNSET
        && (iso_week_of_year != UNSET || iso_year != UNSET || iso_day_of_week != UNSET)
    {
        error!(
            ERR_MIX_ISO_WITH_NATURAL,
            "Mixing of ISO dates with natural dates is not allowed", t.p
        );
    }
    if iso_year != UNSET && (time.y != UNSET || time.m != UNSET || time.d != UNSET) {
        error!(
            ERR_MIX_ISO_WITH_NATURAL,
            "Mixing of ISO dates with natural dates is not allowed", t.p
        );
    }

    if iso_year != UNSET {
        let iw = if iso_week_of_year == UNSET {
            1
        } else {
            iso_week_of_year
        };
        let id = if iso_day_of_week == UNSET {
            1
        } else {
            iso_day_of_week
        };
        (time.y, time.m, time.d) = date_from_isodate(iso_year, iw, id);
    } else if iso_week_of_year != UNSET || iso_day_of_week != UNSET {
        warnings.push(message(
            WARN_INVALID_DATE,
            "The parsed date was invalid",
            &t,
            t.p,
        ));
    }

    if time.h != UNSET && time.i != UNSET && time.s != UNSET && !valid_time(time.h, time.i, time.s)
    {
        warnings.push(message(
            WARN_INVALID_TIME,
            "The parsed time was invalid",
            &t,
            t.p,
        ));
    }
    if time.y != UNSET && time.m != UNSET && time.d != UNSET && !valid_date(time.y, time.m, time.d)
    {
        warnings.push(message(
            WARN_INVALID_DATE,
            "The parsed date was invalid",
            &t,
            t.p,
        ));
    }

    Parsed {
        time,
        errors,
        warnings,
    }
}

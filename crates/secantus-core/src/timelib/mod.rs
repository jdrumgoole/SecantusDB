//! A port of timelib's free-form date parser (`timelib_strtotime`) and of the
//! parts of `timelib_update_ts` it needs -- the parser mongod 8.2.11 uses for
//! `$dateFromString` without a `format` and for `$toDate` / `$convert` of a
//! string.
//!
//! Source: timelib 2022.13 (`parse_date.re`, `tm2unixtime.c`, `dow.c`), the
//! copy vendored by mongod at `src/third_party/timelib/dist`, and mongod's own
//! wrapper `TimeZoneDatabase::fromString` (`date_time_support.cpp`), which turns
//! timelib's errors into the message a client sees.
//!
//! Copyright (c) 2015-2023 Derick Rethans, (c) 2017-2019,2021 MongoDB, Inc.
//! MIT License (`src/timelib/LICENSE-timelib.rst` in this crate). Ported for SecantusDB.
//!
//! The port is deliberately literal: same names, same order of operations,
//! same quirks (a `"T"` alone is military zone T, a leading weekday is a
//! relative jump), because mongod's answer -- including the position and
//! character in every error -- falls out of exactly those steps.

mod format;
mod patterns;
mod scan;
mod update;
mod zones;

pub(crate) use format::validate_format;
pub(crate) use scan::strtotime;
pub(crate) use update::update_ts;

/// timelib's "not set" marker.
pub(crate) const UNSET: i64 = -9_999_999;

pub(crate) const ZONETYPE_OFFSET: i64 = 1;
pub(crate) const ZONETYPE_ABBR: i64 = 2;

const SPECIAL_WEEKDAY: i64 = 0x01;
const SPECIAL_DAY_OF_WEEK_IN_MONTH: i64 = 0x02;
const SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH: i64 = 0x03;
const SPECIAL_FIRST_DAY_OF_MONTH: i64 = 0x01;
const SPECIAL_LAST_DAY_OF_MONTH: i64 = 0x02;

/// The error code mongod rewrites the message for.
pub(crate) const ERR_TZID_NOT_FOUND: i32 = 0x202;

/// `timelib_rel_time`, the fields the parser and `update_ts` use.
#[derive(Debug, Default, Clone)]
pub(crate) struct Relative {
    pub y: i64,
    pub m: i64,
    pub d: i64,
    pub h: i64,
    pub i: i64,
    pub s: i64,
    pub us: i64,
    pub weekday: i64,
    pub weekday_behavior: i64,
    pub first_last_day_of: i64,
    pub special_type: i64,
    pub special_amount: i64,
    pub have_weekday_relative: bool,
    pub have_special_relative: bool,
}

/// `timelib_time`, the fields the parser and `update_ts` use.
#[derive(Debug, Clone)]
pub(crate) struct Time {
    pub y: i64,
    pub m: i64,
    pub d: i64,
    pub h: i64,
    pub i: i64,
    pub s: i64,
    pub us: i64,
    pub z: i64,
    pub dst: i64,
    pub zone_type: i64,
    /// Upper-cased, as `timelib_time_tz_abbr_update` stores it.
    pub tz_abbr: String,
    pub relative: Relative,
    pub have_time: i64,
    pub have_date: bool,
    pub have_zone: i64,
    pub have_relative: bool,
    /// Seconds since the epoch, set by `update_ts`.
    pub sse: i64,
}

impl Time {
    fn unset() -> Self {
        Time {
            y: UNSET,
            m: UNSET,
            d: UNSET,
            h: UNSET,
            i: UNSET,
            s: UNSET,
            us: UNSET,
            z: UNSET,
            dst: UNSET,
            zone_type: 0,
            tz_abbr: String::new(),
            relative: Relative::default(),
            have_time: 0,
            have_date: false,
            have_zone: 0,
            have_relative: false,
            sse: 0,
        }
    }
}

/// One entry of timelib's error or warning list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    pub code: i32,
    pub position: usize,
    pub character: u8,
    pub message: &'static str,
}

/// What `timelib_strtotime` returns.
#[derive(Debug, Clone)]
pub(crate) struct Parsed {
    pub time: Time,
    pub errors: Vec<Message>,
    pub warnings: Vec<Message>,
}

/// mongod's `TimeZoneDatabase::fromString` with no `format`, up to (not
/// including) the time-zone argument: parse, turn any error or warning into
/// mongod's message, default a missing time of day, and refuse a string
/// missing any date or time part. Returns the parsed time, ready for the
/// caller's zone handling and `update_ts`.
pub(crate) fn mongo_parse(text: &str) -> Result<Time, String> {
    from_string(text, strtotime(text.as_bytes()))
}

/// The same with a `format`: `timelib_parse_from_format_with_map` under
/// mongod's map, then the same wrapper. The caller has already validated the
/// format with [`validate_format`].
pub(crate) fn mongo_parse_format(text: &str, format: &str) -> Result<Time, String> {
    from_string(
        text,
        format::parse_from_format(format.as_bytes(), text.as_bytes()),
    )
}

/// `TimeZoneDatabase::fromString` after the timelib call.
fn from_string(text: &str, parsed: Parsed) -> Result<Time, String> {
    if !parsed.errors.is_empty() || !parsed.warnings.is_empty() {
        let mut sb = format!("Error parsing date string '{text}'");
        for e in &parsed.errors {
            sb.push_str(&format!("; {}: ", e.position));
            // mongod never makes zone identifiers available, so it rewrites
            // this one message.
            if e.code == ERR_TZID_NOT_FOUND {
                sb.push_str("passing a time zone identifier as part of the string is not allowed");
            } else {
                sb.push_str(e.message);
            }
            sb.push_str(&format!(" '{}'", e.character as char));
        }
        for w in &parsed.warnings {
            sb.push_str(&format!(
                "; {}: {} '{}'",
                w.position, w.message, w.character as char
            ));
        }
        return Err(sb);
    }
    let mut t = parsed.time;
    // A fully missing time of day is midnight, which lets `%Y-%m-%d` through.
    if t.h == UNSET && t.i == UNSET && t.s == UNSET {
        t.h = 0;
        t.i = 0;
        t.s = 0;
        t.us = 0;
    }
    if [t.y, t.m, t.d, t.h, t.i, t.s].contains(&UNSET) {
        return Err(format!(
            r#"an incomplete date/time string has been found, with elements missing: "{text}""#
        ));
    }
    Ok(t)
}

/// Milliseconds since the epoch of a time `update_ts` has resolved, as mongod
/// computes it: `Seconds(sse) + Microseconds(us)`, converted to milliseconds
/// (truncating toward zero). The seconds are widened to MICROseconds first,
/// so a value beyond about +-292,000 years overflows there -- mongod's 159 --
/// even when it would fit in milliseconds. There is no other bound: year 0
/// and year -100000 are ordinary dates.
pub(crate) fn millis(t: &Time) -> Result<i64, ()> {
    let us = if t.us == UNSET { 0 } else { t.us };
    let micros = t.sse.checked_mul(1_000_000).ok_or(())?;
    Ok(micros.checked_add(us).ok_or(())? / 1000)
}

#[cfg(test)]
mod tests;

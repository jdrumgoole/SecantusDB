//! The datetime functions: `extract` / `date_part`, `date_trunc`, `age`,
//! `justify_*`, `make_*`, `date_bin`, `to_char` over datetimes, and
//! `to_date` / `to_timestamp`.
//!
//! Transcribed from PostgreSQL 14's `timestamp.c`, `date.c` and
//! `formatting.c` (`timestamp_part_common`, `interval_part_common`,
//! `extract_date`, `timestamp_trunc`, `interval_trunc`, `timestamp_age`,
//! `interval_justify_*`, `DCH_to_char`) rather than derived from examples:
//! the year arithmetic around 1 BC, the ISO week rules and the C-division
//! remainders are exactly where an approximation goes wrong.
//!
//! A date, a time and a text all travel as strings, and a `timestamp` and a
//! `timestamptz` as the same instant, so every function here is chosen by
//! the arguments' STATIC types -- the same rule PostgreSQL's function
//! resolution applies.

use bson::Bson;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use crate::{Error, Interval, Result};

const USECS_PER_SEC: i64 = 1_000_000;
const USECS_PER_MINUTE: i64 = 60 * USECS_PER_SEC;
const USECS_PER_HOUR: i64 = 60 * USECS_PER_MINUTE;
const USECS_PER_DAY: i64 = 24 * USECS_PER_HOUR;
const DAYS_PER_MONTH: i32 = 30;

/// The broken-down time PostgreSQL's `pg_tm` holds. `year` is astronomical
/// (0 is 1 BC).
#[derive(Debug, Clone, Copy, Default)]
struct Tm {
    year: i32,
    mon: i32,
    mday: i32,
    hour: i64,
    min: i64,
    sec: i64,
    fsec: i64,
}

fn tm_of(micros: i64) -> Option<Tm> {
    let dt = chrono::DateTime::from_timestamp_micros(micros)?.naive_utc();
    Some(Tm {
        year: dt.year(),
        mon: dt.month() as i32,
        mday: dt.day() as i32,
        hour: i64::from(dt.hour()),
        min: i64::from(dt.minute()),
        sec: i64::from(dt.second()),
        fsec: i64::from(dt.nanosecond() / 1000),
    })
}

fn micros_of(tm: &Tm) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(tm.year, tm.mon as u32, tm.mday as u32)?;
    let t = NaiveTime::from_hms_micro_opt(
        tm.hour as u32,
        tm.min as u32,
        tm.sec as u32,
        tm.fsec as u32,
    )?;
    Some(NaiveDateTime::new(date, t).and_utc().timestamp_micros())
}

/// `date2j`: the Julian day number.
fn date2j(y: i32, m: i32, d: i32) -> i64 {
    let (mut y, mut m) = (i64::from(y), i64::from(m));
    if m > 2 {
        m += 1;
        y += 4800;
    } else {
        m += 13;
        y += 4799;
    }
    let century = y / 100;
    let mut julian = y * 365 - 32167;
    julian += y / 4 - century + century / 4;
    julian += 7834 * m / 256 + i64::from(d);
    julian
}

/// `j2day`: 0 is Sunday.
fn j2day(j: i64) -> i64 {
    (j + 1).rem_euclid(7)
}

fn iso_week(y: i32, m: i32, d: i32) -> (i32, i32) {
    NaiveDate::from_ymd_opt(y, m as u32, d as u32)
        .map(|dt| (dt.iso_week().year(), dt.iso_week().week() as i32))
        .unwrap_or((y, 1))
}

fn isleap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i32, m: i32) -> i32 {
    const DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if m == 2 && isleap(y) {
        29
    } else {
        DAYS[(m - 1).clamp(0, 11) as usize]
    }
}

/// `interval2tm`.
fn interval2tm(iv: &Interval) -> Tm {
    let mut time = iv.micros;
    let hour = time / USECS_PER_HOUR;
    time -= hour * USECS_PER_HOUR;
    let min = time / USECS_PER_MINUTE;
    time -= min * USECS_PER_MINUTE;
    let sec = time / USECS_PER_SEC;
    let fsec = time - sec * USECS_PER_SEC;
    Tm {
        year: iv.months / 12,
        mon: iv.months % 12,
        mday: iv.days,
        hour,
        min,
        sec,
        fsec,
    }
}

/// `tm2interval`.
fn tm2interval(tm: &Tm) -> Interval {
    Interval {
        months: tm.year * 12 + tm.mon,
        days: tm.mday,
        micros: ((tm.hour * 60 + tm.min) * 60 + tm.sec) * USECS_PER_SEC + tm.fsec,
    }
}

// ------------------------------------------------------------------------
// Units
// ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Microsec,
    Millisec,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
    Decade,
    Century,
    Millennium,
    Julian,
    Dow,
    IsoDow,
    Doy,
    IsoYear,
    Tz,
    TzHour,
    TzMinute,
    Epoch,
}

/// `DecodeUnits` / `DecodeSpecial`: tokens compare on their first ten
/// characters (`microseconds` is `microsecon`).
fn unit(name: &str) -> Option<Unit> {
    let low = name.to_ascii_lowercase();
    let t: String = low.chars().take(10).collect();
    Some(match t.as_str() {
        "microsecon" | "us" | "usec" | "usecs" | "useconds" => Unit::Microsec,
        "millisecon" | "ms" | "msec" | "msecs" | "mseconds" => Unit::Millisec,
        "second" | "s" | "sec" | "secs" | "seconds" => Unit::Second,
        "minute" | "m" | "min" | "mins" | "minutes" => Unit::Minute,
        "hour" | "h" | "hr" | "hrs" | "hours" => Unit::Hour,
        "day" | "d" | "days" => Unit::Day,
        "week" | "w" | "weeks" => Unit::Week,
        "month" | "mon" | "mons" | "months" => Unit::Month,
        "quarter" | "qtr" => Unit::Quarter,
        "year" | "y" | "yr" | "yrs" | "years" => Unit::Year,
        "decade" | "dec" | "decs" | "decades" => Unit::Decade,
        "century" | "c" | "cent" | "centuries" => Unit::Century,
        "millennium" | "mil" | "mils" | "millennia" => Unit::Millennium,
        "julian" => Unit::Julian,
        "dow" => Unit::Dow,
        "isodow" => Unit::IsoDow,
        "doy" => Unit::Doy,
        "isoyear" => Unit::IsoYear,
        "timezone" => Unit::Tz,
        "timezone_h" => Unit::TzHour,
        "timezone_m" => Unit::TzMinute,
        "epoch" => Unit::Epoch,
        _ => return None,
    })
}

fn not_recognized(kind: &str, name: &str) -> Error {
    Error::Sqlstate(
        "22023",
        format!(
            "{kind} units \"{}\" not recognized",
            name.to_ascii_lowercase()
        ),
    )
}

fn not_supported(kind: &str, name: &str) -> Error {
    Error::Sqlstate(
        "0A000",
        format!(
            "{kind} units \"{}\" not supported",
            name.to_ascii_lowercase()
        ),
    )
}

/// A part's value: an integer, or `scaled / 10^scale` (seconds and
/// milliseconds are exact decimals).
enum Part {
    Int(i64),
    Scaled(i64, u32),
    Float(f64),
}

fn part_numeric(p: Part) -> Result<Bson> {
    let text = match p {
        Part::Int(i) => i.to_string(),
        Part::Scaled(v, s) => {
            let neg = v < 0;
            let a = v.unsigned_abs().to_string();
            let s = s as usize;
            let padded = format!("{:0>width$}", a, width = s + 1);
            let (i, f) = padded.split_at(padded.len() - s);
            format!("{}{i}.{f}", if neg { "-" } else { "" })
        }
        Part::Float(f) => format!("{f}"),
    };
    crate::cast_value(Bson::String(text), "numeric")
}

fn part_float(p: Part) -> Bson {
    Bson::Double(match p {
        Part::Int(i) => i as f64,
        Part::Scaled(v, s) => v as f64 / 10f64.powi(s as i32),
        Part::Float(f) => f,
    })
}

/// `timestamp_part_common` over a LOCAL broken-down time.
fn timestamp_part(u: Unit, tm: &Tm, name: &str, kind: &str) -> Result<Part> {
    Ok(match u {
        Unit::Microsec => Part::Int(tm.sec * USECS_PER_SEC + tm.fsec),
        Unit::Millisec => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 3),
        Unit::Second => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 6),
        Unit::Minute => Part::Int(tm.min),
        Unit::Hour => Part::Int(tm.hour),
        Unit::Day => Part::Int(i64::from(tm.mday)),
        Unit::Month => Part::Int(i64::from(tm.mon)),
        Unit::Quarter => Part::Int(i64::from((tm.mon - 1) / 3 + 1)),
        Unit::Week => Part::Int(i64::from(iso_week(tm.year, tm.mon, tm.mday).1)),
        Unit::Year => Part::Int(i64::from(if tm.year > 0 { tm.year } else { tm.year - 1 })),
        Unit::Decade => Part::Int(i64::from(if tm.year >= 0 {
            tm.year / 10
        } else {
            -((8 - (tm.year - 1)) / 10)
        })),
        Unit::Century => Part::Int(i64::from(if tm.year > 0 {
            (tm.year + 99) / 100
        } else {
            -((99 - (tm.year - 1)) / 100)
        })),
        Unit::Millennium => Part::Int(i64::from(if tm.year > 0 {
            (tm.year + 999) / 1000
        } else {
            -((999 - (tm.year - 1)) / 1000)
        })),
        Unit::Julian => {
            let day_micros = ((tm.hour * 60 + tm.min) * 60 + tm.sec) * USECS_PER_SEC + tm.fsec;
            let j = date2j(tm.year, tm.mon, tm.mday);
            if day_micros == 0 {
                Part::Int(j)
            } else {
                Part::Float(j as f64 + day_micros as f64 / USECS_PER_DAY as f64)
            }
        }
        Unit::IsoYear => {
            let y = iso_week(tm.year, tm.mon, tm.mday).0;
            Part::Int(i64::from(if y <= 0 { y - 1 } else { y }))
        }
        Unit::Dow | Unit::IsoDow => {
            let d = j2day(date2j(tm.year, tm.mon, tm.mday));
            Part::Int(if u == Unit::IsoDow && d == 0 { 7 } else { d })
        }
        Unit::Doy => Part::Int(date2j(tm.year, tm.mon, tm.mday) - date2j(tm.year, 1, 1) + 1),
        Unit::Tz | Unit::TzHour | Unit::TzMinute | Unit::Epoch => {
            return Err(not_supported(kind, name))
        }
    })
}

/// `extract(julian ...)` as numeric: `date2j + day_micros / 86400000000`,
/// the division at `select_div_scale`'s scale (base-10000 weights, at least
/// 16 significant digits), rounded half away from zero.
fn julian_numeric(tm: &Tm) -> Result<Bson> {
    let j = date2j(tm.year, tm.mon, tm.mday);
    let n = ((tm.hour * 60 + tm.min) * 60 + tm.sec) * USECS_PER_SEC + tm.fsec;
    let d: i64 = 86_400 * USECS_PER_SEC;
    let base_weight = |v: i64| -> (i32, i64) {
        if v == 0 {
            return (0, 0);
        }
        let mut w = 0;
        let mut x = v;
        while x >= 10_000 {
            x /= 10_000;
            w += 1;
        }
        (w, x)
    };
    let (w1, f1) = base_weight(n);
    let (w2, f2) = base_weight(d);
    let mut qweight = w1 - w2;
    if f1 <= f2 {
        qweight -= 1;
    }
    let rscale = (16 - qweight * 4).clamp(0, 1000) as usize;
    // Long division to rscale + 1 digits, then round the last.
    let mut digits = String::new();
    let mut rem = n;
    for _ in 0..=rscale {
        rem *= 10;
        digits.push(char::from(b'0' + (rem / d) as u8));
        rem %= d;
    }
    let mut frac: Vec<u8> = digits.into_bytes();
    let last = frac.pop().unwrap_or(b'0');
    let mut int_part = j;
    if last >= b'5' {
        let mut carry = true;
        for c in frac.iter_mut().rev() {
            if *c == b'9' {
                *c = b'0';
            } else {
                *c += 1;
                carry = false;
                break;
            }
        }
        if carry {
            int_part += 1;
        }
    }
    let text = format!("{int_part}.{}", String::from_utf8(frac).unwrap_or_default());
    crate::cast_value(Bson::String(text), "numeric")
}

/// `extract` (numeric) / `date_part` (float8) over a value of static type
/// `ty`.
pub fn part(name: &str, v: &Bson, ty: &str, numeric: bool) -> Result<Bson> {
    let out = |p: Part| {
        if numeric {
            part_numeric(p)
        } else {
            Ok(part_float(p))
        }
    };
    match ty {
        "interval" => {
            let iv = Interval::from_bson(v).ok_or_else(|| bad("interval", v))?;
            let u = unit(name).ok_or_else(|| not_recognized("interval", name))?;
            let tm = interval2tm(&iv);
            let p = match u {
                Unit::Microsec => Part::Int(tm.sec * USECS_PER_SEC + tm.fsec),
                Unit::Millisec => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 3),
                Unit::Second => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 6),
                Unit::Minute => Part::Int(tm.min),
                Unit::Hour => Part::Int(tm.hour),
                Unit::Day => Part::Int(i64::from(tm.mday)),
                Unit::Month => Part::Int(i64::from(tm.mon)),
                Unit::Quarter => Part::Int(i64::from(tm.mon / 3 + 1)),
                Unit::Year => Part::Int(i64::from(tm.year)),
                Unit::Decade => Part::Int(i64::from(tm.year / 10)),
                Unit::Century => Part::Int(i64::from(tm.year / 100)),
                Unit::Millennium => Part::Int(i64::from(tm.year / 1000)),
                Unit::Epoch => {
                    let secs = (4 * 365 + 1) * i64::from(iv.months / 12) * (86_400 / 4)
                        + i64::from(4 * DAYS_PER_MONTH) * i64::from(iv.months % 12) * (86_400 / 4)
                        + 4 * i64::from(iv.days) * (86_400 / 4);
                    if numeric {
                        Part::Scaled(secs * USECS_PER_SEC + iv.micros, 6)
                    } else {
                        Part::Float(
                            iv.micros as f64 / 1_000_000.0
                                + 365.25 * 86_400.0 * f64::from(iv.months / 12)
                                + 30.0 * 86_400.0 * f64::from(iv.months % 12)
                                + 86_400.0 * f64::from(iv.days),
                        )
                    }
                }
                _ => return Err(not_supported("interval", name)),
            };
            out(p)
        }
        "date" if numeric => {
            let d = date_of(v)?;
            let u = unit(name).ok_or_else(|| not_recognized("date", name))?;
            let tm = Tm {
                year: d.year(),
                mon: d.month() as i32,
                mday: d.day() as i32,
                ..Tm::default()
            };
            match u {
                Unit::Epoch => out(Part::Int(
                    NaiveDateTime::new(d, NaiveTime::MIN).and_utc().timestamp(),
                )),
                Unit::Julian => out(Part::Int(date2j(tm.year, tm.mon, tm.mday))),
                Unit::Microsec
                | Unit::Millisec
                | Unit::Second
                | Unit::Minute
                | Unit::Hour
                | Unit::Tz
                | Unit::TzHour
                | Unit::TzMinute => Err(not_supported("date", name)),
                _ => out(timestamp_part(u, &tm, name, "date")?),
            }
        }
        "time" | "time without time zone" => {
            let t = time_of(v)?;
            let u = unit(name).ok_or_else(|| not_recognized("\"time\"", name))?;
            let micros = i64::from(t.num_seconds_from_midnight()) * USECS_PER_SEC
                + i64::from(t.nanosecond() / 1000);
            let tm = Tm {
                hour: micros / USECS_PER_HOUR,
                min: micros % USECS_PER_HOUR / USECS_PER_MINUTE,
                sec: micros % USECS_PER_MINUTE / USECS_PER_SEC,
                fsec: micros % USECS_PER_SEC,
                ..Tm::default()
            };
            let p = match u {
                Unit::Microsec => Part::Int(tm.sec * USECS_PER_SEC + tm.fsec),
                Unit::Millisec => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 3),
                Unit::Second => Part::Scaled(tm.sec * USECS_PER_SEC + tm.fsec, 6),
                Unit::Minute => Part::Int(tm.min),
                Unit::Hour => Part::Int(tm.hour),
                Unit::Epoch => Part::Scaled(micros, 6),
                _ => return Err(not_supported("\"time\"", name)),
            };
            out(p)
        }
        _ => {
            // timestamp, timestamptz, and a date promoted to timestamp.
            let tz = ty == "timestamptz" || ty == "timestamp with time zone";
            let utc = instant_of(v, ty)?;
            let kind = if tz {
                "timestamp with time zone"
            } else {
                "timestamp"
            };
            let u = unit(name).ok_or_else(|| not_recognized(kind, name))?;
            let offset = if tz {
                i64::from(crate::session_timezone().offset_at(utc).local_minus_utc())
            } else {
                0
            };
            if u == Unit::Epoch {
                return out(Part::Scaled(utc, 6));
            }
            match u {
                Unit::Tz if tz => return out(Part::Int(offset)),
                Unit::TzHour if tz => return out(Part::Int(offset / 3600)),
                Unit::TzMinute if tz => return out(Part::Int(offset / 60 % 60)),
                _ => {}
            }
            let tm = tm_of(utc + offset * USECS_PER_SEC).ok_or_else(out_of_range)?;
            if u == Unit::Julian && numeric {
                return julian_numeric(&tm);
            }
            out(timestamp_part(u, &tm, name, kind)?)
        }
    }
}

fn out_of_range() -> Error {
    Error::DatetimeFieldOverflow("timestamp out of range".into())
}

fn bad(ty: &str, v: &Bson) -> Error {
    Error::InvalidDatetimeFormat(format!(
        "invalid input syntax for type {ty}: \"{}\"",
        crate::value_text(v)
    ))
}

fn date_of(v: &Bson) -> Result<NaiveDate> {
    match v {
        Bson::String(s) => {
            NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| bad("date", v))
        }
        other => instant_of(other, "timestamp")
            .and_then(|m| chrono::DateTime::from_timestamp_micros(m).ok_or_else(out_of_range))
            .map(|d| d.date_naive()),
    }
}

fn time_of(v: &Bson) -> Result<NaiveTime> {
    let s = crate::value_text(v);
    NaiveTime::parse_from_str(s.trim(), "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(s.trim(), "%H:%M"))
        .map_err(|_| bad("time", v))
}

/// A timestamp / timestamptz / date value as UTC microseconds. A `date` is
/// its midnight -- local midnight when promoted to `timestamptz`.
fn instant_of(v: &Bson, ty: &str) -> Result<i64> {
    if let Bson::String(s) = v {
        // A wide-year or BC value travels as UTC text (a `timestamp` as its
        // wall clock): read it whole, fraction and all.
        if let Some((at, frac)) = crate::wide_instant(s.trim()) {
            let digits: String = frac.trim_start_matches('.').chars().take(6).collect();
            let us: i64 = if digits.is_empty() {
                0
            } else {
                format!("{digits:0<6}").parse().unwrap_or(0)
            };
            return Ok(at.and_utc().timestamp_micros() + us);
        }
        // A BC timestamp travels as its text: year N BC is astronomical 1-N.
        if let Some(body) = s.trim().strip_suffix(" BC") {
            let body = body.trim();
            let body = body.strip_suffix("+00").unwrap_or(body);
            let m = crate::parse_timestamp(body)?;
            let dt = chrono::DateTime::from_timestamp_micros(m)
                .ok_or_else(out_of_range)?
                .naive_utc();
            let bc = dt.with_year(1 - dt.year()).ok_or_else(out_of_range)?;
            return Ok(bc.and_utc().timestamp_micros());
        }
        if let Ok(d) = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d") {
            let local = NaiveDateTime::new(d, NaiveTime::MIN)
                .and_utc()
                .timestamp_micros();
            if ty == "timestamptz" || ty == "timestamp with time zone" {
                return Ok(local_to_utc(local));
            }
            return Ok(local);
        }
    }
    crate::instant_micros(v).ok_or_else(|| bad("timestamp", v))
}

/// Local wall-clock microseconds in the session zone to UTC.
fn local_to_utc(local: i64) -> i64 {
    let tz = crate::session_timezone();
    let guess = local - i64::from(tz.offset_at(local).local_minus_utc()) * USECS_PER_SEC;
    local - i64::from(tz.offset_at(guess).local_minus_utc()) * USECS_PER_SEC
}

/// A BC instant as PostgreSQL's text (`0044-03-15 12:00:00 BC`), which is
/// how such a value travels; `None` for an AD one.
fn bc_text(micros: i64, suffix: &str) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp_micros(micros)?.naive_utc();
    if dt.year() >= 1 {
        return None;
    }
    let ad = dt.with_year(1 - dt.year())?;
    let text = crate::render_timestamp(ad.and_utc().timestamp_micros());
    Some(format!("{text}{suffix} BC"))
}

fn timestamp_bson(micros: i64) -> Result<Bson> {
    if let Some(t) = bc_text(micros, "") {
        return Ok(Bson::String(t));
    }
    crate::cast_value(Bson::String(crate::render_timestamp(micros)), "timestamp")
}

fn timestamptz_bson(micros: i64) -> Result<Bson> {
    if let Some(t) = bc_text(micros, "+00") {
        return Ok(Bson::String(t));
    }
    crate::cast_value(
        Bson::String(format!("{}+00", crate::render_timestamp(micros))),
        "timestamptz",
    )
}

// ------------------------------------------------------------------------
// date_trunc, age, justify
// ------------------------------------------------------------------------

/// `date_trunc(unit, value)` over a timestamp / timestamptz / interval.
pub fn trunc(name: &str, v: &Bson, ty: &str) -> Result<Bson> {
    if ty == "interval" {
        let iv = Interval::from_bson(v).ok_or_else(|| bad("interval", v))?;
        let u = unit(name).ok_or_else(|| not_recognized("interval", name))?;
        let mut tm = interval2tm(&iv);
        let step = match u {
            Unit::Millennium => 0,
            Unit::Century => 1,
            Unit::Decade => 2,
            Unit::Year => 3,
            Unit::Quarter => 4,
            Unit::Month => 5,
            Unit::Day => 6,
            Unit::Hour => 7,
            Unit::Minute => 8,
            Unit::Second => 9,
            Unit::Millisec => {
                tm.fsec = tm.fsec / 1000 * 1000;
                return Ok(tm2interval(&tm).to_bson());
            }
            Unit::Microsec => return Ok(tm2interval(&tm).to_bson()),
            Unit::Week => {
                return Err(Error::Sqlstate(
                    "0A000",
                    format!(
                        "interval units \"{}\" not supported because months usually have fractional weeks",
                        name.to_ascii_lowercase()
                    ),
                ))
            }
            _ => return Err(not_supported("interval", name)),
        };
        if step <= 0 {
            tm.year = tm.year / 1000 * 1000;
        }
        if step <= 1 {
            tm.year = tm.year / 100 * 100;
        }
        if step <= 2 {
            tm.year = tm.year / 10 * 10;
        }
        if step <= 3 {
            tm.mon = 0;
        }
        if step <= 4 {
            tm.mon = 3 * (tm.mon / 3);
        }
        if step <= 5 {
            tm.mday = 0;
        }
        if step <= 6 {
            tm.hour = 0;
        }
        if step <= 7 {
            tm.min = 0;
        }
        if step <= 8 {
            tm.sec = 0;
        }
        tm.fsec = 0;
        return Ok(tm2interval(&tm).to_bson());
    }
    let tz = ty == "timestamptz" || ty == "timestamp with time zone";
    let utc = instant_of(v, ty)?;
    let u = unit(name).ok_or_else(|| {
        not_recognized(
            if tz {
                "timestamp with time zone"
            } else {
                "timestamp"
            },
            name,
        )
    })?;
    let offset = if tz {
        i64::from(crate::session_timezone().offset_at(utc).local_minus_utc())
    } else {
        0
    };
    let mut tm = tm_of(utc + offset * USECS_PER_SEC).ok_or_else(out_of_range)?;
    let mut step = match u {
        Unit::Millennium => 0,
        Unit::Century => 1,
        Unit::Decade => 2,
        Unit::Year => 3,
        Unit::Quarter => 4,
        Unit::Month => 5,
        Unit::Day => 6,
        Unit::Hour => 7,
        Unit::Minute => 8,
        Unit::Second => 9,
        Unit::Millisec => 10,
        Unit::Microsec => 11,
        Unit::Week => 12,
        _ => {
            return Err(not_supported(
                if tz {
                    "timestamp with time zone"
                } else {
                    "timestamp"
                },
                name,
            ))
        }
    };
    if step == 12 {
        let (iy, woy) = iso_week(tm.year, tm.mon, tm.mday);
        let monday = NaiveDate::from_isoywd_opt(iy, woy as u32, chrono::Weekday::Mon)
            .ok_or_else(out_of_range)?;
        tm = Tm {
            year: monday.year(),
            mon: monday.month() as i32,
            mday: monday.day() as i32,
            ..Tm::default()
        };
        step = 99;
    }
    if step == 0 {
        tm.year = if tm.year > 0 {
            (tm.year + 999) / 1000 * 1000 - 999
        } else {
            -((999 - (tm.year - 1)) / 1000) * 1000 + 1
        };
    }
    if step <= 1 {
        tm.year = if tm.year > 0 {
            (tm.year + 99) / 100 * 100 - 99
        } else {
            -((99 - (tm.year - 1)) / 100) * 100 + 1
        };
    }
    if step == 2 {
        tm.year = if tm.year > 0 {
            tm.year / 10 * 10
        } else {
            -((8 - (tm.year - 1)) / 10) * 10
        };
    }
    if step <= 3 {
        tm.mon = 1;
    }
    if step <= 4 {
        tm.mon = 3 * ((tm.mon - 1) / 3) + 1;
    }
    if step <= 5 {
        tm.mday = 1;
    }
    if step <= 6 {
        tm.hour = 0;
    }
    if step <= 7 {
        tm.min = 0;
    }
    if step <= 8 {
        tm.sec = 0;
    }
    if step <= 9 {
        tm.fsec = 0;
    }
    if step == 10 {
        tm.fsec = tm.fsec / 1000 * 1000;
    }
    let local = micros_of(&tm).ok_or_else(out_of_range)?;
    if tz {
        // Truncated to a day or coarser, the offset is looked up afresh for
        // the new local time (a DST boundary moves it).
        let utc = if step <= 6 || step == 99 {
            local_to_utc(local)
        } else {
            local - offset * USECS_PER_SEC
        };
        timestamptz_bson(utc)
    } else {
        timestamp_bson(local)
    }
}

/// `age(a, b)`: the symbolic difference, years and months kept.
pub fn age(a: i64, b: i64) -> Result<Bson> {
    let (t1, t2) = (
        tm_of(a).ok_or_else(out_of_range)?,
        tm_of(b).ok_or_else(out_of_range)?,
    );
    let mut fsec = t1.fsec - t2.fsec;
    let mut tm = Tm {
        sec: t1.sec - t2.sec,
        min: t1.min - t2.min,
        hour: t1.hour - t2.hour,
        mday: t1.mday - t2.mday,
        mon: t1.mon - t2.mon,
        year: t1.year - t2.year,
        fsec: 0,
    };
    let flip = a < b;
    let negate = |tm: &mut Tm, fsec: &mut i64| {
        *fsec = -*fsec;
        tm.sec = -tm.sec;
        tm.min = -tm.min;
        tm.hour = -tm.hour;
        tm.mday = -tm.mday;
        tm.mon = -tm.mon;
        tm.year = -tm.year;
    };
    if flip {
        negate(&mut tm, &mut fsec);
    }
    while fsec < 0 {
        fsec += USECS_PER_SEC;
        tm.sec -= 1;
    }
    while tm.sec < 0 {
        tm.sec += 60;
        tm.min -= 1;
    }
    while tm.min < 0 {
        tm.min += 60;
        tm.hour -= 1;
    }
    while tm.hour < 0 {
        tm.hour += 24;
        tm.mday -= 1;
    }
    while tm.mday < 0 {
        let base = if flip { t1 } else { t2 };
        tm.mday += days_in_month(base.year, base.mon);
        tm.mon -= 1;
    }
    while tm.mon < 0 {
        tm.mon += 12;
        tm.year -= 1;
    }
    if flip {
        negate(&mut tm, &mut fsec);
    }
    tm.fsec = fsec;
    Ok(tm2interval(&tm).to_bson())
}

/// `interval_justify_hours`.
pub fn justify_hours(iv: Interval) -> Interval {
    let mut r = iv;
    let whole = r.micros / USECS_PER_DAY;
    r.micros -= whole * USECS_PER_DAY;
    r.days += whole as i32;
    if r.days > 0 && r.micros < 0 {
        r.micros += USECS_PER_DAY;
        r.days -= 1;
    } else if r.days < 0 && r.micros > 0 {
        r.micros -= USECS_PER_DAY;
        r.days += 1;
    }
    r
}

/// `interval_justify_days`.
pub fn justify_days(iv: Interval) -> Interval {
    let mut r = iv;
    let whole = r.days / DAYS_PER_MONTH;
    r.days -= whole * DAYS_PER_MONTH;
    r.months += whole;
    if r.months > 0 && r.days < 0 {
        r.days += DAYS_PER_MONTH;
        r.months -= 1;
    } else if r.months < 0 && r.days > 0 {
        r.days -= DAYS_PER_MONTH;
        r.months += 1;
    }
    r
}

/// `interval_justify_interval`.
pub fn justify_interval(iv: Interval) -> Interval {
    let mut r = iv;
    let whole = r.micros / USECS_PER_DAY;
    r.micros -= whole * USECS_PER_DAY;
    r.days += whole as i32;
    let wm = r.days / DAYS_PER_MONTH;
    r.days -= wm * DAYS_PER_MONTH;
    r.months += wm;
    if r.months > 0 && (r.days < 0 || (r.days == 0 && r.micros < 0)) {
        r.days += DAYS_PER_MONTH;
        r.months -= 1;
    } else if r.months < 0 && (r.days > 0 || (r.days == 0 && r.micros > 0)) {
        r.days -= DAYS_PER_MONTH;
        r.months += 1;
    }
    if r.days > 0 && r.micros < 0 {
        r.micros += USECS_PER_DAY;
        r.days -= 1;
    } else if r.days < 0 && r.micros > 0 {
        r.micros -= USECS_PER_DAY;
        r.days += 1;
    }
    r
}

/// `timestamp - timestamp`: the microsecond difference, hours justified.
pub fn timestamp_diff(a: i64, b: i64) -> Bson {
    justify_hours(Interval {
        months: 0,
        days: 0,
        micros: a - b,
    })
    .to_bson()
}

/// `time +/- interval`, wrapping round the clock.
pub fn time_plus(v: &Bson, iv: &Interval, sign: i64) -> Result<Bson> {
    let t = time_of(v)?;
    let micros =
        i64::from(t.num_seconds_from_midnight()) * USECS_PER_SEC + i64::from(t.nanosecond() / 1000);
    let out = (micros + sign * iv.micros).rem_euclid(USECS_PER_DAY);
    Ok(Bson::String(render_time(out)))
}

/// `time - time`.
pub fn time_diff(a: &Bson, b: &Bson) -> Result<Bson> {
    let m = |t: NaiveTime| {
        i64::from(t.num_seconds_from_midnight()) * USECS_PER_SEC + i64::from(t.nanosecond() / 1000)
    };
    Ok(Interval {
        months: 0,
        days: 0,
        micros: m(time_of(a)?) - m(time_of(b)?),
    }
    .to_bson())
}

fn render_time(micros: i64) -> String {
    let (h, m, s, f) = (
        micros / USECS_PER_HOUR,
        micros % USECS_PER_HOUR / USECS_PER_MINUTE,
        micros % USECS_PER_MINUTE / USECS_PER_SEC,
        micros % USECS_PER_SEC,
    );
    if f == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!(
            "{h:02}:{m:02}:{s:02}.{}",
            format!("{f:06}").trim_end_matches('0')
        )
    }
}

// ------------------------------------------------------------------------
// make_*, to_timestamp(float), date_bin
// ------------------------------------------------------------------------

fn num(v: &Bson) -> Result<f64> {
    match v {
        Bson::Int32(i) => Ok(f64::from(*i)),
        Bson::Int64(i) => Ok(*i as f64),
        Bson::Double(d) => Ok(*d),
        other => crate::value_text(other).trim().parse::<f64>().map_err(|_| {
            Error::InvalidText(format!(
                "invalid input syntax for type double precision: \"{}\"",
                crate::value_text(other)
            ))
        }),
    }
}

fn int(v: &Bson) -> Result<i32> {
    Ok(num(v)? as i32)
}

fn field_range(what: &str, detail: String) -> Error {
    Error::DatetimeFieldOverflow(format!("{what} field value out of range: {detail}"))
}

/// `make_date(y, m, d)`: a negative year is BC.
pub fn make_date(y: i32, m: i32, d: i32) -> Result<NaiveDate> {
    let astro = if y < 0 { y + 1 } else { y };
    if y == 0 {
        return Err(field_range("date", format!("{y}-{m:02}-{d:02}")));
    }
    NaiveDate::from_ymd_opt(astro, m as u32, d as u32)
        .ok_or_else(|| field_range("date", format!("{y}-{m:02}-{d:02}")))
}

fn make_time_micros(h: i32, m: i32, sec: f64) -> Result<i64> {
    let micros = (sec * 1_000_000.0).round() as i64;
    let total = i64::from(h) * USECS_PER_HOUR + i64::from(m) * USECS_PER_MINUTE + micros;
    let in_range = (0..=23).contains(&h) && (0..=59).contains(&m) && (0.0..60.0).contains(&sec);
    let midnight = h == 24 && m == 0 && sec == 0.0;
    if !(in_range || midnight) {
        return Err(field_range("time", format!("{h}:{m:02}:{sec:02}")));
    }
    Ok(total)
}

/// Evaluate one of the constructors. `None`: not one of them.
pub fn make(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !matches!(
        name,
        "make_date" | "make_time" | "make_timestamp" | "make_timestamptz" | "make_interval"
    ) {
        return None;
    }
    if name != "make_interval" && args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    Some((|| match name {
        "make_date" => {
            let [y, m, d] = args else {
                return Err(wrong(name));
            };
            Ok(Bson::String(crate::render_date_pg(make_date(
                int(y)?,
                int(m)?,
                int(d)?,
            )?)))
        }
        "make_time" => {
            let [h, m, s] = args else {
                return Err(wrong(name));
            };
            Ok(Bson::String(render_time(make_time_micros(
                int(h)?,
                int(m)?,
                num(s)?,
            )?)))
        }
        "make_timestamp" | "make_timestamptz" => {
            if args.len() < 6 {
                return Err(wrong(name));
            }
            let d = make_date(int(&args[0])?, int(&args[1])?, int(&args[2])?)?;
            let t = make_time_micros(int(&args[3])?, int(&args[4])?, num(&args[5])?)?;
            let local = NaiveDateTime::new(d, NaiveTime::MIN)
                .and_utc()
                .timestamp_micros()
                + t;
            if name == "make_timestamp" {
                timestamp_bson(local)
            } else {
                let utc = match args.get(6) {
                    Some(z) => {
                        let zone = crate::TimeZoneSetting::parse(&crate::value_text(z));
                        let guess = local
                            - i64::from(zone.offset_at(local).local_minus_utc()) * USECS_PER_SEC;
                        local - i64::from(zone.offset_at(guess).local_minus_utc()) * USECS_PER_SEC
                    }
                    None => local_to_utc(local),
                };
                timestamptz_bson(utc)
            }
        }
        "make_interval" => {
            // (years, months, weeks, days, hours, mins, secs), each optional.
            let get = |i: usize| -> Result<f64> {
                match args.get(i) {
                    None | Some(Bson::Null) => Ok(0.0),
                    Some(v) => num(v),
                }
            };
            if args.contains(&Bson::Null) {
                return Ok(Bson::Null);
            }
            let months = get(0)? as i32 * 12 + get(1)? as i32;
            let days = get(2)? as i32 * 7 + get(3)? as i32;
            let micros = get(4)? as i64 * USECS_PER_HOUR
                + get(5)? as i64 * USECS_PER_MINUTE
                + (get(6)? * 1_000_000.0).round() as i64;
            Ok(Interval {
                months,
                days,
                micros,
            }
            .to_bson())
        }
        _ => Err(wrong(name)),
    })())
}

fn wrong(name: &str) -> Error {
    Error::UndefinedFunction(format!("function {name} does not exist"))
}

/// `to_timestamp(double precision)`: Unix epoch seconds.
pub fn to_timestamp_epoch(v: &Bson) -> Result<Bson> {
    let secs = num(v)?;
    if secs.is_nan() {
        return Err(Error::Sqlstate("22008", "timestamp cannot be NaN".into()));
    }
    if secs.is_infinite() {
        return Ok(Bson::String(
            if secs > 0.0 { "infinity" } else { "-infinity" }.into(),
        ));
    }
    timestamptz_bson((secs * 1_000_000.0).round() as i64)
}

/// `date_bin(stride, source, origin)`.
pub fn date_bin(stride: &Bson, source: i64, origin: i64, tz: bool) -> Result<Bson> {
    let iv = Interval::from_bson(stride).ok_or_else(|| bad("interval", stride))?;
    if iv.months != 0 {
        return Err(Error::Sqlstate(
            "0A000",
            "timestamps cannot be binned into intervals containing months or years".into(),
        ));
    }
    let stride = i64::from(iv.days) * USECS_PER_DAY + iv.micros;
    if stride <= 0 {
        return Err(Error::Sqlstate(
            "22008",
            "stride must be greater than zero".into(),
        ));
    }
    let diff = source - origin;
    let mut delta = diff - diff % stride;
    if diff < 0 && diff % stride != 0 {
        delta -= stride;
    }
    let out = origin + delta;
    if tz {
        timestamptz_bson(out)
    } else {
        timestamp_bson(out)
    }
}

// ------------------------------------------------------------------------
// to_char over datetimes (DCH_to_char)
// ------------------------------------------------------------------------

/// The DCH keywords, named as PostgreSQL's `DCH_*` constants are.
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum K {
    ADp,
    AMp,
    AD,
    AM,
    BCp,
    BC,
    CC,
    DAY,
    DDD,
    DD,
    DY,
    Day,
    Dy,
    D,
    FF(u8),
    FX,
    HH24,
    HH12,
    HH,
    IDDD,
    ID,
    IW,
    IYYY,
    IYY,
    IY,
    I,
    J,
    MI,
    MM,
    MONTH,
    MON,
    MS,
    Month,
    Mon,
    OF,
    PMp,
    PM,
    Q,
    RM,
    SSSS,
    SS,
    TZH,
    TZM,
    TZ,
    US,
    WW,
    W,
    YCOMMA,
    YYYY,
    YYY,
    YY,
    Y,
    adp,
    amp,
    ad,
    am,
    bcp,
    bc,
    day,
    dy,
    month,
    mon,
    pmp,
    pm,
    rm,
    tz,
}

/// PostgreSQL's DCH keyword table, in its search order.
const DCH: &[(&str, K)] = &[
    ("A.D.", K::ADp),
    ("A.M.", K::AMp),
    ("AD", K::AD),
    ("AM", K::AM),
    ("B.C.", K::BCp),
    ("BC", K::BC),
    ("CC", K::CC),
    ("DAY", K::DAY),
    ("DDD", K::DDD),
    ("DD", K::DD),
    ("DY", K::DY),
    ("Day", K::Day),
    ("Dy", K::Dy),
    ("D", K::D),
    ("FF1", K::FF(1)),
    ("FF2", K::FF(2)),
    ("FF3", K::FF(3)),
    ("FF4", K::FF(4)),
    ("FF5", K::FF(5)),
    ("FF6", K::FF(6)),
    ("FX", K::FX),
    ("HH24", K::HH24),
    ("HH12", K::HH12),
    ("HH", K::HH),
    ("IDDD", K::IDDD),
    ("ID", K::ID),
    ("IW", K::IW),
    ("IYYY", K::IYYY),
    ("IYY", K::IYY),
    ("IY", K::IY),
    ("I", K::I),
    ("J", K::J),
    ("MI", K::MI),
    ("MM", K::MM),
    ("MONTH", K::MONTH),
    ("MON", K::MON),
    ("MS", K::MS),
    ("Month", K::Month),
    ("Mon", K::Mon),
    ("OF", K::OF),
    ("P.M.", K::PMp),
    ("PM", K::PM),
    ("Q", K::Q),
    ("RM", K::RM),
    ("SSSSS", K::SSSS),
    ("SSSS", K::SSSS),
    ("SS", K::SS),
    ("TZH", K::TZH),
    ("TZM", K::TZM),
    ("TZ", K::TZ),
    ("US", K::US),
    ("WW", K::WW),
    ("W", K::W),
    ("Y,YYY", K::YCOMMA),
    ("YYYY", K::YYYY),
    ("YYY", K::YYY),
    ("YY", K::YY),
    ("Y", K::Y),
    ("a.d.", K::adp),
    ("a.m.", K::amp),
    ("ad", K::ad),
    ("am", K::am),
    ("b.c.", K::bcp),
    ("bc", K::bc),
    ("cc", K::CC),
    ("day", K::day),
    ("ddd", K::DDD),
    ("dd", K::DD),
    ("dy", K::dy),
    ("d", K::D),
    ("ff1", K::FF(1)),
    ("ff2", K::FF(2)),
    ("ff3", K::FF(3)),
    ("ff4", K::FF(4)),
    ("ff5", K::FF(5)),
    ("ff6", K::FF(6)),
    ("fx", K::FX),
    ("hh24", K::HH24),
    ("hh12", K::HH12),
    ("hh", K::HH),
    ("iddd", K::IDDD),
    ("id", K::ID),
    ("iw", K::IW),
    ("iyyy", K::IYYY),
    ("iyy", K::IYY),
    ("iy", K::IY),
    ("i", K::I),
    ("j", K::J),
    ("mi", K::MI),
    ("mm", K::MM),
    ("month", K::month),
    ("mon", K::mon),
    ("ms", K::MS),
    ("p.m.", K::pmp),
    ("pm", K::pm),
    ("q", K::Q),
    ("rm", K::rm),
    ("sssss", K::SSSS),
    ("ssss", K::SSSS),
    ("ss", K::SS),
    ("tz", K::tz),
    ("us", K::US),
    ("ww", K::WW),
    ("w", K::W),
    ("y,yyy", K::YCOMMA),
    ("yyyy", K::YYYY),
    ("yyy", K::YYY),
    ("yy", K::YY),
    ("y", K::Y),
];

#[derive(Debug, Clone)]
enum DNode {
    Key {
        k: K,
        fm: bool,
        tm: bool,
        th: Option<bool>,
    },
    Lit(String),
}

fn parse_dch(fmt: &str) -> Vec<DNode> {
    let mut out = Vec::new();
    let mut rest = fmt;
    while !rest.is_empty() {
        let mut fm = false;
        let mut tm = false;
        // One prefix only, as suff_search takes the first match.
        if let Some(r) = rest.strip_prefix("FM").or_else(|| rest.strip_prefix("fm")) {
            fm = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("TM").or_else(|| rest.strip_prefix("tm")) {
            tm = true;
            rest = r;
        }
        if let Some((kw, k)) = DCH.iter().find(|(kw, _)| rest.starts_with(kw)) {
            rest = &rest[kw.len()..];
            let mut th = None;
            if let Some(r) = rest.strip_prefix("TH") {
                th = Some(true);
                rest = r;
            } else if let Some(r) = rest.strip_prefix("th") {
                th = Some(false);
                rest = r;
            } else if let Some(r) = rest.strip_prefix("SP") {
                rest = r;
            }
            out.push(DNode::Key { k: *k, fm, tm, th });
            continue;
        }
        if rest.is_empty() {
            break;
        }
        let c = rest.chars().next().expect("non-empty");
        if c == '"' {
            let mut chars = rest[1..].char_indices().peekable();
            let mut end = rest.len();
            while let Some((i, ch)) = chars.next() {
                if ch == '"' {
                    end = i + 2;
                    break;
                }
                if ch == '\\' {
                    if let Some((_, nx)) = chars.next() {
                        out.push(DNode::Lit(nx.to_string()));
                    }
                    continue;
                }
                out.push(DNode::Lit(ch.to_string()));
            }
            rest = &rest[end.min(rest.len())..];
            continue;
        }
        if c == '\\' && rest[1..].starts_with('"') {
            out.push(DNode::Lit("\"".into()));
            rest = &rest[2..];
            continue;
        }
        out.push(DNode::Lit(c.to_string()));
        rest = &rest[c.len_utf8()..];
    }
    out
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const RM_UPPER: [&str; 12] = [
    "XII", "XI", "X", "IX", "VIII", "VII", "VI", "V", "IV", "III", "II", "I",
];

/// The input `DCH_to_char` works on.
struct TmChar {
    tm: Tm,
    wday: i64,
    yday: i64,
    gmtoff: i64,
    zone: Option<String>,
    interval: bool,
}

fn th(num: &str, upper: bool) -> String {
    let b = num.as_bytes();
    let mut last = b.last().copied().unwrap_or(b'0');
    if b.len() > 1 && b[b.len() - 2] == b'1' {
        last = 0;
    }
    let s = match last {
        b'1' => "st",
        b'2' => "nd",
        b'3' => "rd",
        _ => "th",
    };
    format!(
        "{num}{}",
        if upper {
            s.to_ascii_uppercase()
        } else {
            s.to_string()
        }
    )
}

fn pad(v: i64, width: usize, fm: bool) -> String {
    if fm {
        v.to_string()
    } else if v < 0 {
        format!(
            "-{:0>w$}",
            v.unsigned_abs(),
            w = width.saturating_sub(1).max(width - 1)
        )
    } else {
        format!("{v:0width$}")
    }
}

fn adjust_year(y: i32, interval: bool) -> i64 {
    let y = i64::from(y);
    if interval {
        y
    } else if y <= 0 {
        -(y - 1)
    } else {
        y
    }
}

fn invalid_for_interval() -> Error {
    Error::InvalidDatetimeFormat("invalid format specification for an interval value".into())
}

fn dch_to_char(nodes: &[DNode], t: &TmChar) -> Result<String> {
    let tm = &t.tm;
    let mut s = String::new();
    for node in nodes {
        let (k, fm, tmode, thx) = match node {
            DNode::Lit(c) => {
                s.push_str(c);
                continue;
            }
            DNode::Key { k, fm, tm, th } => (*k, *fm, *tm, *th),
        };
        let numth = |txt: String| match thx {
            Some(up) => th(&txt, up),
            None => txt,
        };
        let two = |v: i64| pad(v, if v >= 0 { 2 } else { 3 }, fm);
        let need_date = |s: &mut String| -> Result<()> {
            let _ = s;
            if t.interval {
                Err(invalid_for_interval())
            } else {
                Ok(())
            }
        };
        let pm = tm.hour.rem_euclid(24) >= 12;
        match k {
            K::AMp | K::PMp => s.push_str(if pm { "P.M." } else { "A.M." }),
            K::AM | K::PM => s.push_str(if pm { "PM" } else { "AM" }),
            K::amp | K::pmp => s.push_str(if pm { "p.m." } else { "a.m." }),
            K::am | K::pm => s.push_str(if pm { "pm" } else { "am" }),
            K::HH | K::HH12 => {
                let h = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
                s.push_str(&numth(pad(h, if tm.hour >= 0 { 2 } else { 3 }, fm)));
            }
            K::HH24 => s.push_str(&numth(two(tm.hour))),
            K::MI => s.push_str(&numth(two(tm.min))),
            K::SS => s.push_str(&numth(two(tm.sec))),
            K::FF(n) => {
                let v = tm.fsec / 10i64.pow(6 - u32::from(n));
                s.push_str(&numth(format!("{v:0w$}", w = n as usize)));
            }
            K::MS => s.push_str(&numth(format!("{:03}", tm.fsec / 1000))),
            K::US => s.push_str(&numth(format!("{:06}", tm.fsec))),
            K::SSSS => s.push_str(&numth((tm.hour * 3600 + tm.min * 60 + tm.sec).to_string())),
            K::tz | K::TZ => {
                need_date(&mut s)?;
                if let Some(z) = &t.zone {
                    s.push_str(&if k == K::tz {
                        z.to_ascii_lowercase()
                    } else {
                        z.clone()
                    });
                }
            }
            K::TZH => {
                need_date(&mut s)?;
                s.push_str(&format!(
                    "{}{:02}",
                    if t.gmtoff >= 0 { '+' } else { '-' },
                    t.gmtoff.abs() / 3600
                ));
            }
            K::TZM => {
                need_date(&mut s)?;
                s.push_str(&format!("{:02}", t.gmtoff.abs() % 3600 / 60));
            }
            K::OF => {
                need_date(&mut s)?;
                let h = t.gmtoff.abs() / 3600;
                s.push(if t.gmtoff >= 0 { '+' } else { '-' });
                s.push_str(&if fm { h.to_string() } else { format!("{h:02}") });
                if t.gmtoff.abs() % 3600 != 0 {
                    s.push_str(&format!(":{:02}", t.gmtoff.abs() % 3600 / 60));
                }
            }
            K::ADp | K::BCp => {
                need_date(&mut s)?;
                s.push_str(if tm.year <= 0 { "B.C." } else { "A.D." });
            }
            K::AD | K::BC => {
                need_date(&mut s)?;
                s.push_str(if tm.year <= 0 { "BC" } else { "AD" });
            }
            K::adp | K::bcp => {
                need_date(&mut s)?;
                s.push_str(if tm.year <= 0 { "b.c." } else { "a.d." });
            }
            K::ad | K::bc => {
                need_date(&mut s)?;
                s.push_str(if tm.year <= 0 { "bc" } else { "ad" });
            }
            K::MONTH | K::Month | K::month => {
                need_date(&mut s)?;
                if tm.mon == 0 {
                    continue;
                }
                let name = MONTHS[(tm.mon - 1) as usize];
                let name = match k {
                    K::MONTH => name.to_ascii_uppercase(),
                    K::month => name.to_ascii_lowercase(),
                    _ => name.to_string(),
                };
                s.push_str(&if fm || tmode {
                    name
                } else {
                    format!("{name:<9}")
                });
            }
            K::MON | K::Mon | K::mon => {
                need_date(&mut s)?;
                if tm.mon == 0 {
                    continue;
                }
                let name = &MONTHS[(tm.mon - 1) as usize][..3];
                s.push_str(&match k {
                    K::MON => name.to_ascii_uppercase(),
                    K::mon => name.to_ascii_lowercase(),
                    _ => name.to_string(),
                });
            }
            K::MM => s.push_str(&numth(pad(
                i64::from(tm.mon),
                if tm.mon >= 0 { 2 } else { 3 },
                fm,
            ))),
            K::DAY | K::Day | K::day => {
                need_date(&mut s)?;
                let name = DAYS[t.wday as usize];
                let name = match k {
                    K::DAY => name.to_ascii_uppercase(),
                    K::day => name.to_ascii_lowercase(),
                    _ => name.to_string(),
                };
                s.push_str(&if fm || tmode {
                    name
                } else {
                    format!("{name:<9}")
                });
            }
            K::DY | K::Dy | K::dy => {
                need_date(&mut s)?;
                let name = &DAYS[t.wday as usize][..3];
                s.push_str(&match k {
                    K::DY => name.to_ascii_uppercase(),
                    K::dy => name.to_ascii_lowercase(),
                    _ => name.to_string(),
                });
            }
            K::DDD => s.push_str(&numth(pad(t.yday, 3, fm))),
            K::IDDD => {
                let (iy, w) = iso_week(tm.year, tm.mon, tm.mday);
                let wd = if t.wday == 0 { 7 } else { t.wday };
                let _ = iy;
                s.push_str(&numth(pad(i64::from(w - 1) * 7 + wd, 3, fm)));
            }
            K::DD => s.push_str(&numth(pad(i64::from(tm.mday), 2, fm))),
            K::D => {
                need_date(&mut s)?;
                s.push_str(&numth((t.wday + 1).to_string()));
            }
            K::ID => {
                need_date(&mut s)?;
                s.push_str(&numth((if t.wday == 0 { 7 } else { t.wday }).to_string()));
            }
            K::WW => s.push_str(&numth(pad((t.yday - 1) / 7 + 1, 2, fm))),
            K::IW => s.push_str(&numth(pad(
                i64::from(iso_week(tm.year, tm.mon, tm.mday).1),
                2,
                fm,
            ))),
            K::Q => {
                if tm.mon == 0 {
                    continue;
                }
                s.push_str(&numth(((tm.mon - 1) / 3 + 1).to_string()));
            }
            K::CC => {
                let i = if t.interval {
                    i64::from(tm.year / 100)
                } else if tm.year > 0 {
                    i64::from((tm.year - 1) / 100 + 1)
                } else {
                    i64::from(tm.year / 100 - 1)
                };
                let txt = if (-99..=99).contains(&i) {
                    pad(i, if i >= 0 { 2 } else { 3 }, fm)
                } else {
                    i.to_string()
                };
                s.push_str(&numth(txt));
            }
            K::YCOMMA => {
                let y = adjust_year(tm.year, t.interval);
                let i = y / 1000;
                s.push_str(&numth(format!("{i},{:03}", y - i * 1000)));
            }
            K::YYYY | K::IYYY | K::YYY | K::IYY | K::YY | K::IY | K::Y | K::I => {
                let iso = matches!(k, K::IYYY | K::IYY | K::IY | K::I);
                let base = adjust_year(tm.year, t.interval);
                let y = if iso {
                    adjust_year(iso_week(tm.year, tm.mon, tm.mday).0, t.interval)
                } else {
                    base
                };
                let txt = match k {
                    K::YYYY | K::IYYY => pad(y, if base >= 0 { 4 } else { 5 }, fm),
                    K::YYY | K::IYY => pad(y % 1000, if base >= 0 { 3 } else { 4 }, fm),
                    K::YY | K::IY => pad(y % 100, if base >= 0 { 2 } else { 3 }, fm),
                    _ => (y % 10).to_string(),
                };
                s.push_str(&numth(txt));
            }
            K::RM | K::rm => {
                if tm.mon == 0 && tm.year == 0 {
                    continue;
                }
                let idx = if tm.mon == 0 {
                    if tm.year >= 0 {
                        0
                    } else {
                        11
                    }
                } else if tm.mon < 0 {
                    (-(tm.mon + 1)) as usize
                } else {
                    (12 - tm.mon) as usize
                };
                let r = if k == K::RM {
                    RM_UPPER[idx].to_string()
                } else {
                    RM_UPPER[idx].to_ascii_lowercase()
                };
                s.push_str(&if fm { r } else { format!("{r:<4}") });
            }
            K::W => s.push_str(&numth(((tm.mday - 1) / 7 + 1).to_string())),
            K::J => s.push_str(&numth(date2j(tm.year, tm.mon, tm.mday).to_string())),
            K::FX => {}
        }
    }
    Ok(s)
}

/// `to_char(datetime, format)` over a value of static type `ty`. `None`:
/// not a datetime type.
pub fn to_char(v: &Bson, ty: &str, fmt: &str) -> Option<Result<String>> {
    let nodes = parse_dch(fmt);
    Some((|| {
        let t = match ty {
            "interval" => {
                let iv = Interval::from_bson(v).ok_or_else(|| bad("interval", v))?;
                let tm = interval2tm(&iv);
                let yday =
                    i64::from(tm.year) * 12 * 30 + i64::from(tm.mon) * 30 + i64::from(tm.mday);
                TmChar {
                    tm,
                    wday: 0,
                    yday,
                    gmtoff: 0,
                    zone: None,
                    interval: true,
                }
            }
            "date"
            | "timestamp"
            | "timestamp without time zone"
            | "timestamptz"
            | "timestamp with time zone" => {
                let tz = matches!(ty, "timestamptz" | "timestamp with time zone");
                let utc = instant_of(v, if ty == "date" { "timestamp" } else { ty })?;
                let zone = crate::session_timezone();
                let off = if tz {
                    i64::from(zone.offset_at(utc).local_minus_utc())
                } else {
                    0
                };
                let tm = tm_of(utc + off * USECS_PER_SEC).ok_or_else(out_of_range)?;
                let j = date2j(tm.year, tm.mon, tm.mday);
                TmChar {
                    tm,
                    wday: j2day(j),
                    yday: j - date2j(tm.year, 1, 1) + 1,
                    gmtoff: off,
                    zone: if tz {
                        Some(zone_abbrev(&zone, utc))
                    } else {
                        None
                    },
                    interval: false,
                }
            }
            _ => return Err(Error::Unsupported(format!("to_char() of {ty}"))),
        };
        dch_to_char(&nodes, &t)
    })())
}

fn zone_abbrev(zone: &crate::TimeZoneSetting, utc: i64) -> String {
    use chrono::TimeZone;
    match zone {
        crate::TimeZoneSetting::Utc => "UTC".into(),
        // A POSIX offset zone has no abbreviation (measured: `TZ` prints
        // nothing under `SET timezone = '+05:30'`).
        crate::TimeZoneSetting::Fixed(_) => String::new(),
        crate::TimeZoneSetting::Named(tz) => {
            let instant = chrono::DateTime::from_timestamp_micros(utc).unwrap_or_default();
            tz.from_utc_datetime(&instant.naive_utc())
                .format("%Z")
                .to_string()
        }
    }
}

// ------------------------------------------------------------------------
// to_date / to_timestamp(text, format)
// ------------------------------------------------------------------------

/// Parse `input` by a DCH format into `(date, time-of-day micros, offset
/// seconds if the format read one)`.
pub(crate) fn from_char(input: &str, fmt: &str) -> Result<(NaiveDate, i64, Option<i64>)> {
    from_char_mode(input, fmt, false)
}

/// `from_char` in PostgreSQL's strict ("std") mode, as jsonpath's
/// `.datetime(template)` uses it: every field must be present and nothing may
/// follow the last one.
pub(crate) fn from_char_strict(input: &str, fmt: &str) -> Result<(NaiveDate, i64, Option<i64>)> {
    from_char_mode(input, fmt, true)
}

fn from_char_mode(input: &str, fmt: &str, strict: bool) -> Result<(NaiveDate, i64, Option<i64>)> {
    let nodes = parse_dch(fmt);
    let inp: Vec<char> = input.chars().collect();
    let mut i = 0usize;
    let (mut year, mut mon, mut mday) = (None::<i64>, None::<i64>, None::<i64>);
    let (mut hour, mut min, mut sec, mut fsec) = (0i64, 0i64, 0i64, 0i64);
    let mut pm: Option<bool> = None;
    let mut hh12 = false;
    let mut yday: Option<i64> = None;
    let mut julian: Option<i64> = None;
    let mut bc = false;
    let mut tzh: Option<i64> = None;
    let mut tzm = 0i64;
    let mut cc: Option<i64> = None;
    let mut ydigits = 4usize;
    // ISO 8601 week-numbering: `IYYY` / `IW` / `ID`.
    let mut iso_year = false;
    let mut iso_week: Option<i64> = None;
    let mut iso_day: Option<i64> = None;
    let skip_space = |i: &mut usize| {
        while *i < inp.len() && inp[*i].is_whitespace() {
            *i += 1;
        }
    };
    for (n_idx, node) in nodes.iter().enumerate() {
        // Strict (jsonpath `.datetime()`) parsing: input that runs out while
        // fields remain is an error, where `to_timestamp` defaults them.
        // PostgreSQL stops at the end of the input: the fields left over
        // take their defaults.
        if !strict && i >= inp.len() {
            break;
        }
        if strict && i >= inp.len() {
            if nodes[n_idx..]
                .iter()
                .any(|n| matches!(n, DNode::Key { .. }))
            {
                return Err(Error::Sqlstate(
                    "22007",
                    "input string is too short for datetime format".into(),
                ));
            }
            break;
        }
        match node {
            DNode::Lit(c) => {
                // A literal skips one input character; a separator or space
                // skips any run of separators / spaces there.
                let ch = c.chars().next().unwrap_or(' ');
                if ch.is_whitespace() || "-./,':;".contains(ch) {
                    let mut any = false;
                    while i < inp.len()
                        && (inp[i].is_whitespace() || "-./,':;".contains(inp[i]) || inp[i] == ch)
                    {
                        i += 1;
                        any = true;
                        if !ch.is_whitespace() && inp[i - 1] == ch {
                            break;
                        }
                    }
                    let _ = any;
                } else if i < inp.len() {
                    i += 1;
                }
            }
            DNode::Key { k, fm, .. } => {
                if !matches!(k, K::FX) {
                    skip_space(&mut i);
                }
                // The next node decides whether a number may run long: one
                // followed directly by another numeric field is fixed width.
                let next_numeric = matches!(nodes.get(n_idx + 1), Some(DNode::Key { .. }));
                let fixed = next_numeric && !*fm;
                let kk = *k;
                let read_int = |i: &mut usize, width: usize| read_digits(&inp, i, width, fixed, kk);
                match k {
                    K::YYYY | K::IYYY => {
                        let y = read_int(&mut i, 4)?;
                        year = Some(y);
                        ydigits = 4;
                        iso_year |= *k == K::IYYY;
                    }
                    K::YCOMMA => {
                        let a = read_int(&mut i, 1)?;
                        if i < inp.len() && inp[i] == ',' {
                            i += 1;
                        }
                        let b = read_int(&mut i, 3)?;
                        year = Some(a * 1000 + b);
                    }
                    K::YYY | K::IYY => {
                        year = Some(read_int(&mut i, 3)?);
                        ydigits = 3;
                    }
                    K::YY | K::IY => {
                        year = Some(read_int(&mut i, 2)?);
                        ydigits = 2;
                    }
                    K::Y | K::I => {
                        year = Some(read_int(&mut i, 1)?);
                        ydigits = 1;
                    }
                    K::MM => mon = Some(read_int(&mut i, 2)?),
                    K::DD => mday = Some(read_int(&mut i, 2)?),
                    K::DDD | K::IDDD => yday = Some(read_int(&mut i, 3)?),
                    K::J => julian = Some(read_int(&mut i, 7)?),
                    K::HH | K::HH12 => {
                        hour = read_int(&mut i, 2)?;
                        hh12 = true;
                    }
                    K::HH24 => hour = read_int(&mut i, 2)?,
                    K::MI => min = read_int(&mut i, 2)?,
                    K::SS => sec = read_int(&mut i, 2)?,
                    K::SSSS => {
                        let v = read_int(&mut i, 5)?;
                        hour = v / 3600;
                        min = v / 60 % 60;
                        sec = v % 60;
                    }
                    K::MS | K::FF(_) | K::US => {
                        let (w, scale) = match k {
                            K::MS => (3, 1000),
                            K::US => (6, 1),
                            K::FF(n) => (usize::from(*n), 10i64.pow(6 - u32::from(*n))),
                            _ => unreachable!(),
                        };
                        let start = i;
                        let v = read_int(&mut i, w)?;
                        let len = (i - start) as u32;
                        fsec = if *k == K::MS {
                            v * 10i64.pow(3u32.saturating_sub(len)) * 1000
                        } else if *k == K::US {
                            v * 10i64.pow(6u32.saturating_sub(len))
                        } else {
                            v * scale
                        };
                    }
                    K::CC => cc = Some(read_int(&mut i, 2)?),
                    K::AM | K::PM | K::am | K::pm | K::AMp | K::PMp | K::amp | K::pmp => {
                        let rest: String = inp[i..]
                            .iter()
                            .take(4)
                            .collect::<String>()
                            .to_ascii_uppercase();
                        if rest.starts_with("A.M.") || rest.starts_with("P.M.") {
                            pm = Some(rest.starts_with('P'));
                            i += 4;
                        } else if rest.starts_with("AM") || rest.starts_with("PM") {
                            pm = Some(rest.starts_with('P'));
                            i += 2;
                        } else {
                            return Err(not_allowed(&inp[i..], &key_name(*k)));
                        }
                    }
                    K::AD | K::BC | K::ad | K::bc | K::ADp | K::BCp | K::adp | K::bcp => {
                        let rest: String = inp[i..]
                            .iter()
                            .take(4)
                            .collect::<String>()
                            .to_ascii_uppercase();
                        if rest.starts_with("B.C.") || rest.starts_with("A.D.") {
                            bc = rest.starts_with('B');
                            i += 4;
                        } else if rest.starts_with("BC") || rest.starts_with("AD") {
                            bc = rest.starts_with('B');
                            i += 2;
                        }
                    }
                    K::MONTH | K::Month | K::month | K::MON | K::Mon | K::mon | K::RM | K::rm => {
                        let rest: String = inp[i..].iter().collect::<String>().to_ascii_lowercase();
                        let full = matches!(k, K::MONTH | K::Month | K::month);
                        let mut found = None;
                        if matches!(k, K::RM | K::rm) {
                            for (idx, r) in RM_UPPER.iter().enumerate() {
                                if rest.to_ascii_uppercase().starts_with(r) {
                                    found = Some(((12 - idx) as i64, r.len()));
                                    break;
                                }
                            }
                        } else {
                            for (idx, m) in MONTHS.iter().enumerate() {
                                let m = m.to_ascii_lowercase();
                                if full && rest.starts_with(&m) {
                                    found = Some((idx as i64 + 1, m.len()));
                                    break;
                                }
                                if rest.starts_with(&m[..3]) {
                                    found = Some((idx as i64 + 1, 3));
                                    if !full {
                                        break;
                                    }
                                }
                            }
                        }
                        let (m, len) =
                            found.ok_or_else(|| not_allowed(&inp[i..], &key_name(*k)))?;
                        mon = Some(m);
                        i += len;
                    }
                    K::DAY | K::Day | K::day | K::DY | K::Dy | K::dy => {
                        let rest: String = inp[i..].iter().collect::<String>().to_ascii_lowercase();
                        let full = matches!(k, K::DAY | K::Day | K::day);
                        let mut len = 0;
                        for d in DAYS {
                            let d = d.to_ascii_lowercase();
                            if full && rest.starts_with(&d) {
                                len = d.len();
                                break;
                            }
                            if rest.starts_with(&d[..3]) {
                                len = 3;
                            }
                        }
                        if len == 0 {
                            return Err(not_allowed(&inp[i..], &key_name(*k)));
                        }
                        i += len;
                    }
                    K::IW => iso_week = Some(read_int(&mut i, 2)?),
                    K::ID => iso_day = Some(read_int(&mut i, 1)?),
                    K::D | K::W | K::WW | K::Q => {
                        read_int(&mut i, 2)?;
                    }
                    K::TZH => {
                        let v = read_int(&mut i, 2)?;
                        tzh = Some(v);
                    }
                    K::TZM => tzm = read_int(&mut i, 2)?,
                    K::TZ | K::tz | K::OF => {
                        return Err(Error::Sqlstate(
                            "0A000",
                            format!(
                                "formatting field \"{}\" is only supported in to_char",
                                key_name(*k)
                            ),
                        ))
                    }
                    K::FX => {}
                }
            }
        }
    }
    if strict {
        while i < inp.len() && inp[i].is_whitespace() {
            i += 1;
        }
        if i < inp.len() {
            return Err(Error::Sqlstate(
                "22007",
                "trailing characters remain in input string after datetime format".into(),
            ));
        }
    }
    if hh12 || pm.is_some() {
        if !(1..=12).contains(&hour) {
            return Err(Error::InvalidDatetimeFormat(format!(
                "hour \"{hour}\" is invalid for the 12-hour clock"
            )));
        }
        if pm == Some(true) && hour < 12 {
            hour += 12;
        } else if pm == Some(false) && hour == 12 {
            hour = 0;
        }
    }
    let mut y = match (year, cc) {
        (Some(y), _) if (2..4).contains(&ydigits) => {
            // Two- and three-digit years pick the nearest century to 2020.
            if ydigits == 2 {
                if y < 70 {
                    y + 2000
                } else {
                    y + 1900
                }
            } else if y < 100 {
                y + 2000
            } else {
                y + 1000
            }
        }
        (Some(y), _) if ydigits == 1 => y + 2000,
        (Some(y), _) => y,
        (None, Some(c)) => (c - 1) * 100 + 1,
        // No year field: year zero, which is 1 BC.
        (None, None) => 0,
    };
    // `do_to_timestamp`: BC negates, and a year at or below zero is one
    // off from the proleptic count (there is no year 0).
    if bc {
        y = -y;
    }
    if y < 0 {
        y += 1;
    }
    // A zero month, day or day-of-year is "not given", as
    // `do_to_timestamp`'s `if (tmfc.mm)` has it: `to_date('2020-00-10',
    // ...)` is January 10th.
    let mon = mon.filter(|m| *m != 0);
    let mday = mday.filter(|d| *d != 0);
    let yday = yday.filter(|d| *d != 0);
    // `isoweek2j`: day 1 of ISO week 1 is the Monday of the week holding
    // January 4th; `ID` counts Monday 1 .. Sunday 7.
    let iso = iso_week.filter(|_| iso_year).map(|w| {
        let day4 = date2j(y as i32, 1, 4);
        let day0 = (day4 - 1 + 1).rem_euclid(7);
        (w - 1) * 7 + (day4 - day0) + iso_day.map_or(0, |d| (d - 1).rem_euclid(7))
    });
    let date = if let Some(j) = iso {
        let days = j - date2j(2000, 1, 1);
        NaiveDate::from_ymd_opt(2000, 1, 1)
            .and_then(|d| d.checked_add_signed(chrono::Duration::days(days)))
    } else if let Some(j) = julian {
        let days = j - date2j(2000, 1, 1);
        NaiveDate::from_ymd_opt(2000, 1, 1)
            .and_then(|d| d.checked_add_signed(chrono::Duration::days(days)))
    } else if let (Some(yd), None) = (yday, mon) {
        NaiveDate::from_yo_opt(y as i32, yd as u32)
    } else {
        let m = mon.unwrap_or(1);
        let d = mday.unwrap_or(1);
        if !(1..=12).contains(&m) {
            return Err(Error::DatetimeFieldOverflow(format!(
                "date/time field value out of range: \"{input}\""
            )));
        }
        NaiveDate::from_ymd_opt(y as i32, m as u32, d as u32)
    }
    .ok_or_else(|| {
        Error::DatetimeFieldOverflow(format!("date/time field value out of range: \"{input}\""))
    })?;
    if hour > 24 || min > 59 || sec > 60 {
        return Err(Error::DatetimeFieldOverflow(format!(
            "date/time field value out of range: \"{input}\""
        )));
    }
    let time = hour * USECS_PER_HOUR + min * USECS_PER_MINUTE + sec * USECS_PER_SEC + fsec;
    let offset = tzh.map(|h| h * 3600 + if h < 0 { -tzm * 60 } else { tzm * 60 });
    Ok((date, time, offset))
}

/// Read a field's digits (with an optional sign): at most `width` when the
/// field is fixed-width, else as many as there are.
fn read_digits(inp: &[char], i: &mut usize, width: usize, fixed: bool, k: K) -> Result<i64> {
    let start = *i;
    let mut j = *i;
    if j < inp.len() && (inp[j] == '-' || inp[j] == '+') {
        j += 1;
    }
    let limit = if fixed { width } else { usize::MAX };
    let digits_start = j;
    while j < inp.len() && inp[j].is_ascii_digit() && j - digits_start < limit {
        j += 1;
    }
    if j == digits_start {
        let found: String = inp[start..].iter().take(width.max(1)).collect();
        return Err(Error::InvalidDatetimeFormat(format!(
            "invalid value \"{found}\" for \"{}\"\nDetail: Value must be an integer.",
            key_name(k)
        )));
    }
    *i = j;
    inp[start..j]
        .iter()
        .collect::<String>()
        .parse::<i64>()
        .map_err(|_| out_of_range())
}

fn key_name(k: K) -> String {
    DCH.iter()
        .find(|(_, kk)| *kk == k)
        .map_or("?".into(), |(n, _)| (*n).to_string())
}

/// `to_date(text, format)`.
pub fn to_date(input: &str, fmt: &str) -> Result<Bson> {
    let (d, _, _) = from_char(input, fmt)?;
    Ok(Bson::String(crate::render_date_pg(d)))
}

/// `to_timestamp(text, format)`: a timestamptz, read in the session zone
/// unless the format supplied an offset.
pub fn to_timestamp_text(input: &str, fmt: &str) -> Result<Bson> {
    let (d, t, off) = from_char(input, fmt)?;
    let local = NaiveDateTime::new(d, NaiveTime::MIN)
        .and_utc()
        .timestamp_micros()
        + t;
    let utc = match off {
        Some(o) => local - o * USECS_PER_SEC,
        None => local_to_utc(local),
    };
    timestamptz_bson(utc)
}

// ------------------------------------------------------------------------
// Dispatch
// ------------------------------------------------------------------------

/// The functions routed here, with their static result types.
/// The zone an `AT TIME ZONE` names: a zone name, an abbreviation (`EST`,
/// offset east), a POSIX-style offset (`+05`, `utc+3` -- hours WEST, the
/// reverse of an ISO offset), or an interval (east). 22023 otherwise.
fn zone_of(zone: &Bson) -> Result<crate::TimeZoneSetting> {
    let fixed = |east: i64| -> Result<crate::TimeZoneSetting> {
        i32::try_from(east)
            .ok()
            .and_then(chrono::FixedOffset::east_opt)
            .map(crate::TimeZoneSetting::Fixed)
            .ok_or_else(|| Error::InvalidParameter("time zone displacement out of range".into()))
    };
    if let Some(iv) = crate::Interval::from_bson(zone) {
        if iv.months != 0 || iv.days != 0 {
            return Err(Error::InvalidParameter(format!(
                "interval time zone \"{}\" must not include months or days",
                crate::interval_value_text(zone).unwrap_or_default()
            )));
        }
        return fixed(iv.micros / 1_000_000);
    }
    let text = crate::value_text(zone);
    let t = text.trim();
    if t.eq_ignore_ascii_case("utc") || t.eq_ignore_ascii_case("gmt") || t.eq_ignore_ascii_case("z")
    {
        return Ok(crate::TimeZoneSetting::Utc);
    }
    if let Some(tz) = crate::dtparse::resolve_zone(t) {
        return Ok(crate::TimeZoneSetting::Named(tz));
    }
    if let Some(east) = crate::dtparse::abbreviation_offset(t) {
        return fixed(i64::from(east));
    }
    // POSIX: an optional name, then [+-]hh[:mm[:ss]], positive meaning WEST.
    let body = t.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let (sign, digits) = match body.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, body.strip_prefix('+').unwrap_or(body)),
    };
    let parts: Vec<&str> = digits.split(':').collect();
    if !digits.is_empty()
        && parts.len() <= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        let n: Vec<i64> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
        let west =
            n[0] * 3600 + n.get(1).copied().unwrap_or(0) * 60 + n.get(2).copied().unwrap_or(0);
        return fixed(-sign * west);
    }
    Err(Error::InvalidParameter(format!(
        "time zone \"{t}\" not recognized"
    )))
}

/// `timezone(zone, ts)`: a `timestamptz` read as the wall clock in `zone`
/// (a `timestamp`), or a `timestamp` taken as wall clock in `zone` (a
/// `timestamptz`).
fn at_time_zone(zone: &Bson, ts: &Bson, ts_type: &str) -> Result<Bson> {
    if *zone == Bson::Null || *ts == Bson::Null {
        return Ok(Bson::Null);
    }
    let tz = zone_of(zone)?;
    if matches!(ts_type, "timestamp" | "timestamp without time zone") {
        let local =
            crate::instant_micros(ts).ok_or_else(|| Error::Unsupported("this timestamp".into()))?;
        let offset = tz.offset_for_local(local).local_minus_utc();
        return Ok(crate::timestamptz_value_from_micros(
            local - i64::from(offset) * 1_000_000,
        ));
    }
    let utc = match crate::instant_micros(ts) {
        Some(m) => m,
        None => crate::instant_micros(&crate::cast_value(ts.clone(), "timestamptz")?)
            .ok_or_else(|| Error::Unsupported("this timestamp".into()))?,
    };
    let offset = tz.offset_at(utc).local_minus_utc();
    Ok(crate::timestamptz_value_from_micros(
        utc + i64::from(offset) * 1_000_000,
    ))
}

pub fn result_type(name: &str, arg_types: &[String]) -> Option<String> {
    let first = arg_types.first().map(String::as_str).unwrap_or("");
    let second = arg_types.get(1).map(String::as_str).unwrap_or("");
    // `ts AT TIME ZONE z` is `timezone(z, ts)`, and it SWAPS the zone-ness.
    if name == "timezone" && arg_types.len() == 2 {
        return Some(
            if matches!(second, "timestamp" | "timestamp without time zone") {
                "timestamptz".to_string()
            } else {
                "timestamp".to_string()
            },
        );
    }
    Some(
        match name {
            "extract" => "numeric",
            "date_part" => "float8",
            "date_trunc" => match second {
                "interval" => "interval",
                "timestamp" | "timestamp without time zone" => "timestamp",
                _ => "timestamptz",
            },
            "date_bin" => match second {
                "timestamp" | "timestamp without time zone" => "timestamp",
                _ => "timestamptz",
            },
            "age" if arg_types.len() == 2 || arg_types.len() == 1 => "interval",
            "justify_days" | "justify_hours" | "justify_interval" | "make_interval" => "interval",
            "make_date" | "to_date" => "date",
            "make_time" => "time",
            "make_timestamp" => "timestamp",
            "make_timestamptz" | "to_timestamp" => "timestamptz",
            "isfinite" => "bool",
            "to_char" if is_datetime(first) => "text",
            _ => return None,
        }
        .to_string(),
    )
}

pub fn is_datetime(t: &str) -> bool {
    matches!(
        t,
        "date"
            | "timestamp"
            | "timestamptz"
            | "timestamp without time zone"
            | "timestamp with time zone"
            | "interval"
            | "time"
            | "time without time zone"
    )
}

/// Evaluate a datetime function, given its argument values and static
/// types. `None`: not one of these.
pub fn call(name: &str, args: &[Bson], types: &[String]) -> Option<Result<Bson>> {
    if let Some(r) = make(name, args) {
        return Some(r);
    }
    if name == "timezone" && args.len() == 2 {
        return Some(at_time_zone(
            &args[0],
            &args[1],
            types.get(1).map_or("", String::as_str),
        ));
    }
    let t = |i: usize| types.get(i).map(String::as_str).unwrap_or("");
    let strict = |args: &[Bson]| args.contains(&Bson::Null);
    Some(match name {
        "extract" | "date_part" => {
            let [u, v] = args else { return None };
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            let ty = if t(1) == "text" || t(1).is_empty() {
                "timestamptz"
            } else {
                t(1)
            };
            let ty = if name == "date_part" && ty == "date" {
                "timestamp"
            } else {
                ty
            };
            part(&crate::value_text(u), v, ty, name == "extract")
        }
        "date_trunc" => {
            if args.len() < 2 || args.len() > 3 {
                return None;
            }
            if strict(&args[..2]) {
                return Some(Ok(Bson::Null));
            }
            let ty = match t(1) {
                "interval" => "interval",
                "timestamp" | "timestamp without time zone" => "timestamp",
                _ => "timestamptz",
            };
            if let Some(z) = args.get(2) {
                // date_trunc(unit, timestamptz, zone): truncate in that zone.
                let zone = crate::TimeZoneSetting::parse(&crate::value_text(z));
                let prev = crate::PLAN_TIMEZONE.with(|p| p.replace(zone));
                let out = trunc(&crate::value_text(&args[0]), &args[1], ty);
                crate::PLAN_TIMEZONE.with(|p| *p.borrow_mut() = prev);
                out
            } else {
                trunc(&crate::value_text(&args[0]), &args[1], ty)
            }
        }
        "date_bin" => {
            let [s, v, o] = args else { return None };
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            let tz = !matches!(t(1), "timestamp" | "timestamp without time zone");
            let ty = if tz { "timestamptz" } else { "timestamp" };
            (|| {
                // An untyped literal takes the parameter's type.
                let s = if Interval::from_bson(s).is_none() {
                    crate::cast_value(s.clone(), "interval")?
                } else {
                    s.clone()
                };
                let coerce = |v: &Bson| -> Result<i64> {
                    match v {
                        Bson::String(_) => instant_of(&crate::cast_value(v.clone(), ty)?, ty),
                        other => instant_of(other, ty),
                    }
                };
                date_bin(&s, coerce(v)?, coerce(o)?, tz)
            })()
        }
        "age" => {
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            match args {
                [a, b] => {
                    let ty = if t(0) == "date" { "timestamptz" } else { t(0) };
                    (|| {
                        let tz = matches!(ty, "timestamptz" | "timestamp with time zone");
                        let (x, y) = (instant_of(a, ty)?, instant_of(b, ty)?);
                        if tz {
                            let zone = crate::session_timezone();
                            let off = |m: i64| {
                                i64::from(zone.offset_at(m).local_minus_utc()) * USECS_PER_SEC
                            };
                            age(x + off(x), y + off(y))
                        } else {
                            age(x, y)
                        }
                    })()
                }
                [a] => (|| {
                    let ty = if t(0) == "date" { "timestamptz" } else { t(0) };
                    let x = instant_of(a, ty)?;
                    let now = crate::instant_micros(&crate::scalar::now_value()).unwrap_or(0);
                    let tz = matches!(ty, "timestamptz" | "timestamp with time zone");
                    let zone = crate::session_timezone();
                    let off = |m: i64| {
                        if tz {
                            i64::from(zone.offset_at(m).local_minus_utc()) * USECS_PER_SEC
                        } else {
                            0
                        }
                    };
                    let local_now =
                        now + i64::from(zone.offset_at(now).local_minus_utc()) * USECS_PER_SEC;
                    let midnight = local_now - local_now.rem_euclid(USECS_PER_DAY);
                    age(midnight, x + off(x))
                })(),
                _ => return None,
            }
        }
        "justify_days" | "justify_hours" | "justify_interval" => {
            let [v] = args else { return None };
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            match Interval::from_bson(v) {
                Some(iv) => Ok(match name {
                    "justify_days" => justify_days(iv),
                    "justify_hours" => justify_hours(iv),
                    _ => justify_interval(iv),
                }
                .to_bson()),
                None => Err(bad("interval", v)),
            }
        }
        "to_timestamp" => {
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            match args {
                [v] => to_timestamp_epoch(v),
                [s, f] => to_timestamp_text(&crate::value_text(s), &crate::value_text(f)),
                _ => return None,
            }
        }
        "to_date" => {
            let [s, f] = args else { return None };
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            to_date(&crate::value_text(s), &crate::value_text(f))
        }
        "to_char" if is_datetime(t(0)) => {
            let [v, f] = args else { return None };
            if strict(args) {
                return Some(Ok(Bson::Null));
            }
            // No to_char(date): a date is promoted to timestamptz, the
            // preferred datetime type.
            let ty = match t(0) {
                "time" | "time without time zone" => "interval",
                "date" => "timestamptz",
                other => other,
            };
            let v = if ty == "interval" && Interval::from_bson(v).is_none() {
                match time_of(v) {
                    Ok(tm) => Interval {
                        months: 0,
                        days: 0,
                        micros: i64::from(tm.num_seconds_from_midnight()) * USECS_PER_SEC
                            + i64::from(tm.nanosecond() / 1000),
                    }
                    .to_bson(),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                v.clone()
            };
            to_char(&v, ty, &crate::value_text(f))?.map(Bson::String)
        }
        "isfinite" => {
            let [v] = args else { return None };
            Ok(match v {
                Bson::Null => Bson::Null,
                Bson::String(s) if s.contains("infinity") => Bson::Boolean(false),
                _ => Bson::Boolean(true),
            })
        }
        _ => return None,
    })
}

/// `from_char_seq_search`'s failure: the input as written, up to the next
/// whitespace, and the field.
fn not_allowed(rest: &[char], field: &str) -> Error {
    let word: String = rest.iter().take_while(|c| !c.is_whitespace()).collect();
    Error::InvalidDatetimeFormat(format!(
        "invalid value \"{word}\" for \"{field}\"\nDetail: The given value did not match any of the allowed values for this field."
    ))
}

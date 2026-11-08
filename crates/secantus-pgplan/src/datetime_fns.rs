//! `extract` / `date_part` and `date_trunc`.
//!
//! Every value and rendering here was measured against PostgreSQL 16.15:
//! `extract` answers a `numeric` whose scale depends on the FIELD (`second`
//! and `epoch` carry six decimals, `milliseconds` three, `julian` twenty over
//! a timestamp and none over a date, everything else none); `date_part` is
//! the same number as a `float8`. `date_trunc` keeps the operand's type
//! except over a `date`, which PostgreSQL first promotes to `timestamptz`.

use crate::{
    instant_micros, numeric::parse_numeric, render_timestamp, session_timezone, time_of_day_micros,
    timestamptz_value_from_micros, Error, Interval, Result,
};
use bson::Bson;
use chrono::{Datelike, NaiveDate, Timelike};

const MICROS_PER_SEC: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SEC;
/// Julian day number of 2000-01-01, the `julian` field's origin.
const JULIAN_2000: i64 = 2_451_545;
/// Unix seconds at 2000-01-01T00:00:00Z.
const EPOCH_2000_SECS: i64 = 946_684_800;

/// The datetime family an operand belongs to, from its DECLARED type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Timestamp,
    Timestamptz,
    Date,
    Time,
    Interval,
}

impl Kind {
    pub fn of_type(ty: &str) -> Option<Kind> {
        Some(match ty {
            "timestamp" | "timestamp without time zone" => Kind::Timestamp,
            "timestamptz" | "timestamp with time zone" => Kind::Timestamptz,
            "date" => Kind::Date,
            "time" | "time without time zone" => Kind::Time,
            "interval" => Kind::Interval,
            _ => return None,
        })
    }

    fn display(self) -> &'static str {
        match self {
            Kind::Timestamp => "timestamp without time zone",
            Kind::Timestamptz => "timestamp with time zone",
            Kind::Date => "date",
            Kind::Time => "time without time zone",
            Kind::Interval => "interval",
        }
    }
}

fn not_recognized(unit: &str, kind: Kind) -> Error {
    Error::InvalidParameter(format!(
        "unit \"{unit}\" not recognized for type {}",
        kind.display()
    ))
}

fn not_supported(unit: &str, kind: Kind) -> Error {
    Error::FeatureNotSupported(format!(
        "unit \"{unit}\" not supported for type {}",
        kind.display()
    ))
}

/// A wall-clock reading in microseconds since the Unix epoch, as a chrono
/// naive datetime.
fn naive(micros: i64) -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::from_timestamp_micros(micros).map(|d| d.naive_utc())
}

/// Floor division of microseconds, correct for readings before the epoch.
fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// A field of a timestamp reading (wall-clock micros), of a date (no time
/// fields), or of a timestamptz, whose `offset` from UTC in seconds answers
/// the `timezone` fields and moves `epoch` back to the instant. `None` is
/// "not recognized"; `Some(None)` is a field the type has no answer for.
fn timestamp_field(
    unit: &str,
    micros: i64,
    is_date: bool,
    offset: Option<i64>,
) -> Option<Option<Field>> {
    let dt = naive(micros)?;
    let date = dt.date();
    let time_field = |f: Field| if is_date { Some(None) } else { Some(Some(f)) };
    let year = i64::from(date.year());
    let century = if year > 0 {
        (year - 1).div_euclid(100) + 1
    } else {
        -((-year).div_euclid(100) + 1)
    };
    let seconds_of_day = i64::from(dt.num_seconds_from_midnight());
    let micros_of_day = seconds_of_day * MICROS_PER_SEC + i64::from(dt.nanosecond() / 1000);
    let secs_field = micros_of_day.rem_euclid(60 * MICROS_PER_SEC);
    Some(Some(match unit {
        "year" => Field::Int(year),
        "month" => Field::Int(i64::from(date.month())),
        "day" => Field::Int(i64::from(date.day())),
        "quarter" => Field::Int(i64::from((date.month() - 1) / 3 + 1)),
        "week" => Field::Int(i64::from(date.iso_week().week())),
        "isoyear" => Field::Int(i64::from(date.iso_week().year())),
        "dow" => Field::Int(i64::from(date.weekday().num_days_from_sunday())),
        "isodow" => Field::Int(i64::from(date.weekday().number_from_monday())),
        "doy" => Field::Int(i64::from(date.ordinal())),
        "decade" => Field::Int(year.div_euclid(10)),
        "century" => Field::Int(century),
        "millennium" => Field::Int(if year > 0 {
            (year - 1).div_euclid(1000) + 1
        } else {
            -((-year).div_euclid(1000) + 1)
        }),
        "epoch" => {
            let instant = micros - offset.unwrap_or(0) * MICROS_PER_SEC;
            if is_date {
                Field::Int(floor_div(instant, MICROS_PER_SEC))
            } else {
                Field::Scaled(instant, 6)
            }
        }
        "julian" => {
            let days = floor_div(micros, MICROS_PER_DAY) + JULIAN_2000
                - floor_div(EPOCH_2000_SECS * MICROS_PER_SEC, MICROS_PER_DAY);
            if is_date {
                Field::Int(days)
            } else {
                Field::JulianFraction(days, micros.rem_euclid(MICROS_PER_DAY))
            }
        }
        "hour" => return time_field(Field::Int(i64::from(dt.hour()))),
        "minute" => return time_field(Field::Int(i64::from(dt.minute()))),
        "second" => return time_field(Field::Scaled(secs_field, 6)),
        "milliseconds" => return time_field(Field::Scaled(secs_field, 3)),
        "microseconds" => return time_field(Field::Int(secs_field)),
        "timezone" | "timezone_hour" | "timezone_minute" => {
            let Some(off) = offset else {
                return Some(None);
            };
            Field::Int(match unit {
                "timezone" => off,
                "timezone_hour" => off / 3600,
                _ => (off % 3600) / 60,
            })
        }
        _ => return None,
    }))
}

/// A field of a time-of-day reading. `None` is "not recognized";
/// `Some(None)` is a date field, which a time has no answer for.
fn time_field(unit: &str, micros: i64) -> Option<Option<Field>> {
    let secs_field = micros.rem_euclid(60 * MICROS_PER_SEC);
    Some(Some(match unit {
        "hour" => Field::Int(micros / (3600 * MICROS_PER_SEC)),
        "minute" => Field::Int((micros / (60 * MICROS_PER_SEC)) % 60),
        "second" => Field::Scaled(secs_field, 6),
        "milliseconds" => Field::Scaled(secs_field, 3),
        "microseconds" => Field::Int(secs_field),
        "epoch" => Field::Scaled(micros, 6),
        "year" | "month" | "day" | "quarter" | "week" | "isoyear" | "dow" | "isodow" | "doy"
        | "decade" | "century" | "millennium" | "julian" | "timezone" | "timezone_hour"
        | "timezone_minute" => return Some(None),
        _ => return None,
    }))
}

/// A field of an interval. `None` is "not recognized"; `Some(None)` is a
/// field the type refuses (`week`).
fn interval_field(unit: &str, iv: &Interval) -> Option<Option<Field>> {
    let months = i64::from(iv.months);
    // Interval fields keep their sign: `-2.5 sec` is `-2.5`, not `57.5`.
    let secs_field = iv.micros % (60 * MICROS_PER_SEC);
    let years_trunc = months / 12;
    Some(Some(match unit {
        "year" => Field::Int(years_trunc),
        "month" => Field::Int(months % 12),
        "day" => Field::Int(i64::from(iv.days)),
        "hour" => Field::Int(iv.micros / (3600 * MICROS_PER_SEC)),
        "minute" => Field::Int((iv.micros / (60 * MICROS_PER_SEC)) % 60),
        "second" => Field::Scaled(secs_field, 6),
        "milliseconds" => Field::Scaled(secs_field, 3),
        "microseconds" => Field::Int(secs_field),
        "quarter" => Field::Int((months % 12) / 3 + 1),
        "decade" => Field::Int(years_trunc / 10),
        "century" => Field::Int(years_trunc / 100),
        "millennium" => Field::Int(years_trunc / 1000),
        // PostgreSQL counts a year as 365.25 days and a month as 30 here.
        "epoch" => Field::Scaled(
            years_trunc * 31_557_600 * MICROS_PER_SEC
                + (months % 12) * 30 * MICROS_PER_DAY
                + i64::from(iv.days) * MICROS_PER_DAY
                + iv.micros,
            6,
        ),
        "week" | "julian" | "dow" | "isodow" | "doy" | "isoyear" | "timezone" | "timezone_hour"
        | "timezone_minute" => return Some(None),
        _ => return None,
    }))
}

/// One extracted field before rendering: an integer, a count of microseconds
/// shown as `micros / 10^scale` with `scale` decimals (six for seconds, three
/// for milliseconds), or the julian day with its day fraction.
enum Field {
    Int(i64),
    Scaled(i64, u32),
    JulianFraction(i64, i64),
}

impl Field {
    fn numeric_text(&self) -> String {
        match self {
            Field::Int(v) => v.to_string(),
            Field::Scaled(micros, scale) => {
                let divisor = 10_u64.pow(*scale);
                let magnitude = micros.unsigned_abs();
                let units = magnitude / divisor;
                let frac = magnitude % divisor;
                let sign = if *micros < 0 { "-" } else { "" };
                format!("{sign}{units}.{frac:0width$}", width = *scale as usize)
            }
            Field::JulianFraction(days, micros_of_day) => {
                // 20 decimals, the fraction being micros / 86_400_000_000.
                let frac: u128 = u128::from(micros_of_day.unsigned_abs()) * 10u128.pow(20)
                    / u128::from(MICROS_PER_DAY.unsigned_abs());
                format!("{days}.{frac:020}")
            }
        }
    }

    fn float(&self) -> f64 {
        match self {
            Field::Int(v) => *v as f64,
            Field::Scaled(micros, scale) => *micros as f64 / 10_f64.powi(*scale as i32),
            Field::JulianFraction(days, micros) => {
                *days as f64 + *micros as f64 / MICROS_PER_DAY as f64
            }
        }
    }
}

/// The value the operand carries as a wall-clock reading: the naive micros
/// of a timestamp, the session-zone reading of a timestamptz instant, or
/// midnight of a date.
fn wall_clock_micros(value: &Bson, kind: Kind) -> Option<(i64, Option<i64>)> {
    let instant = instant_micros(value)?;
    Some(match kind {
        Kind::Timestamptz => {
            let off = i64::from(session_timezone().offset_at(instant).local_minus_utc());
            (instant + off * MICROS_PER_SEC, Some(off))
        }
        _ => (instant, None),
    })
}

/// `extract(unit from value)` (a `numeric`) or, with `as_float`,
/// `date_part(unit, value)` (a `float8`).
pub fn extract(unit: &str, value: &Bson, kind: Kind, as_float: bool) -> Result<Bson> {
    if *value == Bson::Null {
        return Ok(Bson::Null);
    }
    let unit_lc = unit.to_ascii_lowercase();
    let field = match kind {
        Kind::Interval => {
            let iv = Interval::from_bson(value).ok_or_else(|| not_recognized(unit, kind))?;
            interval_field(&unit_lc, &iv)
        }
        Kind::Time => {
            let t = time_of_day_micros(value).ok_or_else(|| not_recognized(unit, kind))?;
            time_field(&unit_lc, t)
        }
        Kind::Timestamp | Kind::Timestamptz | Kind::Date => {
            let (micros, offset) =
                wall_clock_micros(value, kind).ok_or_else(|| not_recognized(unit, kind))?;
            timestamp_field(&unit_lc, micros, kind == Kind::Date, offset)
        }
    };
    let field = match field {
        None => return Err(not_recognized(unit, kind)),
        Some(None) => return Err(not_supported(unit, kind)),
        Some(Some(f)) => f,
    };
    if as_float {
        Ok(Bson::Double(field.float()))
    } else {
        parse_numeric(&field.numeric_text())
    }
}

/// The wall-clock reading truncated to `unit`, or `None` for a unit the
/// type has no truncation for.
fn truncate_micros(unit: &str, micros: i64) -> Option<i64> {
    let dt = naive(micros)?;
    let date = dt.date();
    let day_start = |d: NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .map(|n| n.and_utc().timestamp_micros())
    };
    let year = date.year();
    Some(match unit {
        "microseconds" => micros,
        "milliseconds" => floor_div(micros, 1000) * 1000,
        "second" => floor_div(micros, MICROS_PER_SEC) * MICROS_PER_SEC,
        "minute" => floor_div(micros, 60 * MICROS_PER_SEC) * 60 * MICROS_PER_SEC,
        "hour" => floor_div(micros, 3600 * MICROS_PER_SEC) * 3600 * MICROS_PER_SEC,
        "day" => day_start(date)?,
        "week" => {
            let back = i64::from(date.weekday().num_days_from_monday());
            day_start(date)? - back * MICROS_PER_DAY
        }
        "month" => day_start(NaiveDate::from_ymd_opt(year, date.month(), 1)?)?,
        "quarter" => {
            let m = (date.month() - 1) / 3 * 3 + 1;
            day_start(NaiveDate::from_ymd_opt(year, m, 1)?)?
        }
        "year" => day_start(NaiveDate::from_ymd_opt(year, 1, 1)?)?,
        "decade" => day_start(NaiveDate::from_ymd_opt(year.div_euclid(10) * 10, 1, 1)?)?,
        "century" => {
            let y = if year > 0 {
                (year - 1).div_euclid(100) * 100 + 1
            } else {
                year
            };
            day_start(NaiveDate::from_ymd_opt(y, 1, 1)?)?
        }
        "millennium" => {
            let y = if year > 0 {
                (year - 1).div_euclid(1000) * 1000 + 1
            } else {
                year
            };
            day_start(NaiveDate::from_ymd_opt(y, 1, 1)?)?
        }
        _ => return None,
    })
}

fn truncate_interval(unit: &str, iv: &Interval) -> Option<Option<Interval>> {
    let m = iv.months;
    let us = iv.micros;
    let years = m / 12;
    Some(Some(match unit {
        "microseconds" => *iv,
        "milliseconds" => Interval {
            micros: us / 1000 * 1000,
            ..*iv
        },
        "second" => Interval {
            micros: us / MICROS_PER_SEC * MICROS_PER_SEC,
            ..*iv
        },
        "minute" => Interval {
            micros: us / (60 * MICROS_PER_SEC) * 60 * MICROS_PER_SEC,
            ..*iv
        },
        "hour" => Interval {
            micros: us / (3600 * MICROS_PER_SEC) * 3600 * MICROS_PER_SEC,
            ..*iv
        },
        "day" => Interval { micros: 0, ..*iv },
        "month" => Interval {
            months: m,
            days: 0,
            micros: 0,
        },
        "quarter" => Interval {
            months: years * 12 + (m % 12) / 3 * 3,
            days: 0,
            micros: 0,
        },
        "year" => Interval {
            months: years * 12,
            days: 0,
            micros: 0,
        },
        "decade" => Interval {
            months: years / 10 * 120,
            days: 0,
            micros: 0,
        },
        "century" => Interval {
            months: years / 100 * 1200,
            days: 0,
            micros: 0,
        },
        "millennium" => Interval {
            months: years / 1000 * 12000,
            days: 0,
            micros: 0,
        },
        "week" => return Some(None),
        _ => return None,
    }))
}

/// `date_trunc(unit, value)`. The result keeps the operand's type -- a
/// timestamp text, a timestamptz instant, an interval -- except that a date
/// is promoted to a timestamptz first, as PostgreSQL does.
pub fn date_trunc(unit: &str, value: &Bson, kind: Kind) -> Result<Bson> {
    if *value == Bson::Null {
        return Ok(Bson::Null);
    }
    let unit_lc = unit.to_ascii_lowercase();
    match kind {
        Kind::Interval => {
            let iv = Interval::from_bson(value).ok_or_else(|| not_recognized(unit, kind))?;
            match truncate_interval(&unit_lc, &iv) {
                None => Err(not_recognized(unit, kind)),
                Some(None) => Err(not_supported(unit, kind)),
                Some(Some(out)) => Ok(out.to_bson()),
            }
        }
        // PostgreSQL has no `date_trunc` over a time and casts it to an
        // INTERVAL, which is also what comes back.
        Kind::Time => {
            let t =
                time_of_day_micros(value).ok_or_else(|| not_recognized(unit, Kind::Interval))?;
            date_trunc(
                unit,
                &Interval {
                    months: 0,
                    days: 0,
                    micros: t,
                }
                .to_bson(),
                Kind::Interval,
            )
        }
        Kind::Timestamp => {
            let micros = instant_micros(value).ok_or_else(|| not_recognized(unit, kind))?;
            let out =
                truncate_micros(&unit_lc, micros).ok_or_else(|| not_recognized(unit, kind))?;
            Ok(Bson::String(render_timestamp(out)))
        }
        Kind::Timestamptz | Kind::Date => {
            let tz = session_timezone();
            let instant = match kind {
                // A date is midnight in the session zone.
                Kind::Date => {
                    let naive = instant_micros(value).ok_or_else(|| not_recognized(unit, kind))?;
                    naive - i64::from(tz.offset_for_local(naive).local_minus_utc()) * MICROS_PER_SEC
                }
                _ => instant_micros(value).ok_or_else(|| not_recognized(unit, kind))?,
            };
            let local =
                instant + i64::from(tz.offset_at(instant).local_minus_utc()) * MICROS_PER_SEC;
            let out_local = truncate_micros(&unit_lc, local)
                .ok_or_else(|| not_recognized(unit, Kind::Timestamptz))?;
            let out = out_local
                - i64::from(tz.offset_for_local(out_local).local_minus_utc()) * MICROS_PER_SEC;
            Ok(timestamptz_value_from_micros(out))
        }
    }
}

/// The declared result type of `date_trunc` over an operand of `kind`.
pub fn date_trunc_type(kind: Kind) -> &'static str {
    match kind {
        Kind::Interval => "interval",
        Kind::Timestamp => "timestamp",
        Kind::Time => "interval",
        _ => "timestamptz",
    }
}

/// Days in `month` (1-based) of `year`.
fn days_in_month(year: i32, month: u32) -> i64 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    match (next, NaiveDate::from_ymd_opt(year, month, 1)) {
        (Some(n), Some(f)) => (n - f).num_days(),
        _ => 30,
    }
}

/// `age(later, earlier)`: PostgreSQL's `timestamp_age`, a FIELD-WISE
/// difference of the two wall-clock readings (year, month, day, hour,
/// minute, second, microsecond) with negative fields borrowed upward -- a
/// short day count borrows the length of the EARLIER reading's month, which
/// is why `age('2024-03-01', '2023-02-28')` is `1 year 1 day` and not the
/// 367 days the instants are apart. Both operands are read in the session
/// zone when they are `timestamptz`; a `date` reads as its midnight.
pub fn age(later: &Bson, later_kind: Kind, earlier: &Bson, earlier_kind: Kind) -> Result<Bson> {
    if *later == Bson::Null || *earlier == Bson::Null {
        return Ok(Bson::Null);
    }
    let (Some((l, _)), Some((e, _))) = (
        wall_clock_micros(later, later_kind),
        wall_clock_micros(earlier, earlier_kind),
    ) else {
        return Ok(Bson::Null);
    };
    let (Some(lt), Some(et)) = (naive(l), naive(e)) else {
        return Ok(Bson::Null);
    };
    let flipped = l < e;
    let mut usec = i64::from(lt.and_utc().timestamp_subsec_micros())
        - i64::from(et.and_utc().timestamp_subsec_micros());
    let mut sec = i64::from(lt.second()) - i64::from(et.second());
    let mut min = i64::from(lt.minute()) - i64::from(et.minute());
    let mut hour = i64::from(lt.hour()) - i64::from(et.hour());
    let mut mday = i64::from(lt.day()) - i64::from(et.day());
    let mut mon = i64::from(lt.month()) - i64::from(et.month());
    let mut year = i64::from(lt.year()) - i64::from(et.year());
    if flipped {
        for f in [
            &mut usec, &mut sec, &mut min, &mut hour, &mut mday, &mut mon, &mut year,
        ] {
            *f = -*f;
        }
    }
    while usec < 0 {
        usec += MICROS_PER_SEC;
        sec -= 1;
    }
    while sec < 0 {
        sec += 60;
        min -= 1;
    }
    while min < 0 {
        min += 60;
        hour -= 1;
    }
    while hour < 0 {
        hour += 24;
        mday -= 1;
    }
    // The borrowed month is the EARLIER reading's -- which is `later` once
    // the sign has flipped.
    let borrow = if flipped { &lt } else { &et };
    while mday < 0 {
        mday += days_in_month(borrow.year(), borrow.month());
        mon -= 1;
    }
    while mon < 0 {
        mon += 12;
        year -= 1;
    }
    if flipped {
        for f in [
            &mut usec, &mut sec, &mut min, &mut hour, &mut mday, &mut mon, &mut year,
        ] {
            *f = -*f;
        }
    }
    let months = i32::try_from(year * 12 + mon)
        .map_err(|_| Error::InvalidParameter("interval out of range".into()))?;
    let days =
        i32::try_from(mday).map_err(|_| Error::InvalidParameter("interval out of range".into()))?;
    let micros = ((hour * 60 + min) * 60 + sec) * MICROS_PER_SEC + usec;
    Ok(Interval {
        months,
        days,
        micros,
    }
    .to_bson())
}

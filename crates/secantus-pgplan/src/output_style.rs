//! The session settings that change how values are WRITTEN as text
//! (RP-068): `IntervalStyle`, `bytea_output` and `extra_float_digits`.
//! Installed per thread with the DateStyle, by the wire layer, and read by
//! the output functions (`render_interval`, `bytea::render_hex`,
//! `geo::float8_text` / `float4_text`).

use std::cell::Cell;

/// `IntervalStyle`'s four values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntervalStyle {
    #[default]
    Postgres,
    PostgresVerbose,
    SqlStandard,
    Iso8601,
}

impl IntervalStyle {
    /// The setting's value as `SET` stores it; anything else is the default.
    pub fn parse(v: &str) -> IntervalStyle {
        match v.trim().to_ascii_lowercase().as_str() {
            "postgres_verbose" => IntervalStyle::PostgresVerbose,
            "sql_standard" => IntervalStyle::SqlStandard,
            "iso_8601" => IntervalStyle::Iso8601,
            _ => IntervalStyle::Postgres,
        }
    }
}

/// The session's output settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputStyle {
    pub interval: IntervalStyle,
    /// `bytea_output = escape`.
    pub bytea_escape: bool,
    /// `extra_float_digits`, -15 to 3; 1 and above is the shortest exact text.
    pub extra_float_digits: i32,
}

impl Default for OutputStyle {
    fn default() -> Self {
        OutputStyle {
            interval: IntervalStyle::Postgres,
            bytea_escape: false,
            extra_float_digits: 1,
        }
    }
}

thread_local! {
    static STYLE: Cell<OutputStyle> = Cell::new(OutputStyle::default());
}

/// Install the session's output settings for the statements that follow.
pub fn set_output_style(style: OutputStyle) {
    STYLE.with(|s| s.set(style));
}

pub(crate) fn output_style() -> OutputStyle {
    STYLE.with(Cell::get)
}

/// `byteaout` under `bytea_output = escape`: printable ASCII as itself, a
/// backslash doubled, every other byte as `\ooo` octal.
pub(crate) fn bytea_escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out
}

/// C's `%.{precision}g` for a finite, nonzero `v`: `precision` significant
/// digits, trailing zeros dropped, fixed notation for a decimal exponent in
/// `-4..precision` and `d.ddde+XX` otherwise -- `float8out` /
/// `float4out` below `extra_float_digits = 1`.
pub(crate) fn format_g(v: f64, precision: i32) -> String {
    let p = precision.max(1) as usize;
    let sci = format!("{:.*e}", p - 1, v);
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("an integer exponent");
    if exp < -4 || exp >= p as i32 {
        let m = trim_fraction(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{m}e{sign}{:02}", exp.abs());
    }
    let decimals = (p as i32 - 1 - exp).max(0) as usize;
    trim_fraction(&format!("{:.*}", decimals, v)).to_string()
}

fn trim_fraction(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// One interval, already split as `interval2itm` splits it: years, months,
/// days, hours, minutes, seconds and microseconds, each carrying its own
/// sign (the time fields all share the time's sign).
#[derive(Clone, Copy)]
pub(crate) struct ItmParts {
    pub year: i32,
    pub mon: i32,
    pub mday: i32,
    pub hour: i64,
    pub min: i32,
    pub sec: i32,
    pub fsec: i32,
}

/// `AppendSeconds`: the seconds' magnitude, two digits when `fill`, then a
/// fraction with its trailing zeros dropped.
fn append_seconds(out: &mut String, sec: i32, fsec: i32, fill: bool) {
    if fill {
        out.push_str(&format!("{:02}", sec.unsigned_abs()));
    } else {
        out.push_str(&sec.unsigned_abs().to_string());
    }
    if fsec != 0 {
        out.push('.');
        out.push_str(format!("{:06}", fsec.unsigned_abs()).trim_end_matches('0'));
    }
}

/// `EncodeInterval`, every style (`datetime.c`, PostgreSQL 15).
pub(crate) fn encode_interval(t: &ItmParts, style: IntervalStyle) -> String {
    let ItmParts {
        mut year,
        mut mon,
        mut mday,
        mut hour,
        mut min,
        mut sec,
        mut fsec,
    } = *t;
    let mut out = String::new();
    match style {
        IntervalStyle::Iso8601 => {
            if year == 0 && mon == 0 && mday == 0 && hour == 0 && min == 0 && sec == 0 && fsec == 0
            {
                return "PT0S".into();
            }
            out.push('P');
            let part = |out: &mut String, v: i64, unit: char| {
                if v != 0 {
                    out.push_str(&format!("{v}{unit}"));
                }
            };
            part(&mut out, i64::from(year), 'Y');
            part(&mut out, i64::from(mon), 'M');
            part(&mut out, i64::from(mday), 'D');
            if hour != 0 || min != 0 || sec != 0 || fsec != 0 {
                out.push('T');
            }
            part(&mut out, hour, 'H');
            part(&mut out, i64::from(min), 'M');
            if sec != 0 || fsec != 0 {
                if sec < 0 || fsec < 0 {
                    out.push('-');
                }
                append_seconds(&mut out, sec, fsec, false);
                out.push('S');
            }
        }
        IntervalStyle::SqlStandard => {
            let has_negative =
                year < 0 || mon < 0 || mday < 0 || hour < 0 || min < 0 || sec < 0 || fsec < 0;
            let has_positive =
                year > 0 || mon > 0 || mday > 0 || hour > 0 || min > 0 || sec > 0 || fsec > 0;
            let has_year_month = year != 0 || mon != 0;
            let has_day_time = mday != 0 || hour != 0 || min != 0 || sec != 0 || fsec != 0;
            let has_day = mday != 0;
            let standard = !(has_negative && has_positive) && !(has_year_month && has_day_time);
            if has_negative && standard {
                out.push('-');
                year = -year;
                mon = -mon;
                mday = -mday;
                hour = -hour;
                min = -min;
                sec = -sec;
                fsec = -fsec;
            }
            if !has_negative && !has_positive {
                out.push('0');
            } else if !standard {
                let year_sign = if year < 0 || mon < 0 { '-' } else { '+' };
                let day_sign = if mday < 0 { '-' } else { '+' };
                let sec_sign = if hour < 0 || min < 0 || sec < 0 || fsec < 0 {
                    '-'
                } else {
                    '+'
                };
                out.push_str(&format!(
                    "{year_sign}{}-{} {day_sign}{} {sec_sign}{}:{:02}:",
                    year.unsigned_abs(),
                    mon.unsigned_abs(),
                    mday.unsigned_abs(),
                    hour.unsigned_abs(),
                    min.unsigned_abs()
                ));
                append_seconds(&mut out, sec, fsec, true);
            } else if has_year_month {
                out.push_str(&format!("{year}-{mon}"));
            } else if has_day {
                out.push_str(&format!("{mday} {hour}:{min:02}:"));
                append_seconds(&mut out, sec, fsec, true);
            } else {
                out.push_str(&format!("{hour}:{min:02}:"));
                append_seconds(&mut out, sec, fsec, true);
            }
        }
        IntervalStyle::PostgresVerbose => {
            out.push('@');
            let mut is_zero = true;
            let mut is_before = false;
            let mut part = |out: &mut String, value: i64, units: &str| {
                if value == 0 {
                    return;
                }
                let mut value = value;
                if is_zero {
                    is_before = value < 0;
                    value = value.abs();
                } else if is_before {
                    value = -value;
                }
                out.push_str(&format!(
                    " {value} {units}{}",
                    if value == 1 { "" } else { "s" }
                ));
                is_zero = false;
            };
            part(&mut out, i64::from(year), "year");
            part(&mut out, i64::from(mon), "mon");
            part(&mut out, i64::from(mday), "day");
            part(&mut out, hour, "hour");
            part(&mut out, i64::from(min), "min");
            if sec != 0 || fsec != 0 {
                out.push(' ');
                if sec < 0 || (sec == 0 && fsec < 0) {
                    if is_zero {
                        is_before = true;
                    } else if !is_before {
                        out.push('-');
                    }
                } else if is_before {
                    out.push('-');
                }
                append_seconds(&mut out, sec, fsec, false);
                out.push_str(&format!(
                    " sec{}",
                    if sec.unsigned_abs() != 1 || fsec != 0 {
                        "s"
                    } else {
                        ""
                    }
                ));
                is_zero = false;
            }
            if is_zero {
                out.push_str(" 0");
            }
            if is_before {
                out.push_str(" ago");
            }
        }
        IntervalStyle::Postgres => {
            let mut is_zero = true;
            let mut is_before = false;
            let mut part = |out: &mut String, value: i64, units: &str| {
                if value == 0 {
                    return;
                }
                out.push_str(&format!(
                    "{}{}{value} {units}{}",
                    if is_zero { "" } else { " " },
                    if is_before && value > 0 { "+" } else { "" },
                    if value != 1 { "s" } else { "" }
                ));
                is_before = value < 0;
                is_zero = false;
            };
            part(&mut out, i64::from(year), "year");
            part(&mut out, i64::from(mon), "mon");
            part(&mut out, i64::from(mday), "day");
            if is_zero || hour != 0 || min != 0 || sec != 0 || fsec != 0 {
                let minus = hour < 0 || min < 0 || sec < 0 || fsec < 0;
                out.push_str(&format!(
                    "{}{}{:02}:{:02}:",
                    if is_zero { "" } else { " " },
                    if minus {
                        "-"
                    } else if is_before {
                        "+"
                    } else {
                        ""
                    },
                    hour.unsigned_abs(),
                    min.unsigned_abs()
                ));
                append_seconds(&mut out, sec, fsec, true);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_g_is_c_printf() {
        assert_eq!(format_g(1.0 / 3.0, 15), "0.333333333333333");
        assert_eq!(format_g(1.0 / 3.0, 14), "0.33333333333333");
        assert_eq!(format_g(0.30000000000000004, 15), "0.3");
        assert_eq!(format_g(1e20, 15), "1e+20");
        assert_eq!(format_g(123456.0, 6), "123456");
        assert_eq!(format_g(1234567.0, 6), "1.23457e+06");
        assert_eq!(format_g(0.0001, 6), "0.0001");
        assert_eq!(format_g(0.00001, 6), "1e-05");
    }

    #[test]
    fn bytea_escape_matches_byteaout() {
        assert_eq!(bytea_escape(&[0x41, 0xff, b'\\', 0x00]), "A\\377\\\\\\000");
    }
}

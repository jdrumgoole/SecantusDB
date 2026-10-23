//! A parameter's value as PostgreSQL 15 reads and shows it:
//! `parse_and_validate_value` for a bool, integer, real or enum parameter
//! (`parse_bool`, `parse_int`, `parse_real`, `config_enum_lookup_by_name`
//! and the range check), and `_ShowOption` for the text `SHOW` then answers.
//! What a parameter is comes from the `pg_settings` dump
//! (`pg15_settings.tsv`): its type, unit, bounds and enum values.
//!
//! A value is stored as its `SHOW` text (`set work_mem = 8192` stores
//! `8MB`), the form every reader of a setting already takes.

use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::pg15_settings::{pg15_setting, Pg15Setting};

/// A unit of the memory or time table: its name and its size in the
/// table's own measure (bytes, or milliseconds).
type Unit = (&'static str, f64);

/// `memory_unit_conversion_table`, largest first.
const MEMORY_UNITS: [Unit; 5] = [
    ("TB", 1024.0 * 1024.0 * 1024.0 * 1024.0),
    ("GB", 1024.0 * 1024.0 * 1024.0),
    ("MB", 1024.0 * 1024.0),
    ("kB", 1024.0),
    ("B", 1.0),
];

/// `time_unit_conversion_table`, largest first.
const TIME_UNITS: [Unit; 6] = [
    ("d", 86_400_000.0),
    ("h", 3_600_000.0),
    ("min", 60_000.0),
    ("s", 1_000.0),
    ("ms", 1.0),
    ("us", 0.001),
];

const MEMORY_HINT: &str =
    "Valid units for this parameter are \"B\", \"kB\", \"MB\", \"GB\", and \"TB\".";
const TIME_HINT: &str =
    "Valid units for this parameter are \"us\", \"ms\", \"s\", \"min\", \"h\", and \"d\".";

/// The hidden spellings an enum parameter also takes (`config_enum_entry`s
/// marked hidden), each with the visible name it means: the boolean words
/// for the parameters that grew out of a boolean (measured on 15.19).
fn hidden_enum_value(name: &str, value: &str) -> Option<&'static str> {
    let on = matches!(value, "true" | "yes" | "1");
    let off = matches!(value, "false" | "no" | "0");
    match name {
        "archive_mode"
        | "backslash_quote"
        | "compute_query_id"
        | "constraint_exclusion"
        | "force_parallel_mode"
        | "huge_pages"
        | "recovery_prefetch"
        | "synchronous_commit" => {
            if on {
                Some("on")
            } else if off {
                Some("off")
            } else {
                None
            }
        }
        // `on` and its spellings are `pglz`.
        "wal_compression" => {
            if on || value == "on" {
                Some("pglz")
            } else if off {
                Some("off")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A parameter's base unit: which table, and the base's size in it.
fn base_unit(unit: &str) -> Option<(&'static [Unit], f64, &'static str)> {
    Some(match unit {
        "B" => (&MEMORY_UNITS, 1.0, MEMORY_HINT),
        "kB" => (&MEMORY_UNITS, 1024.0, MEMORY_HINT),
        "8kB" => (&MEMORY_UNITS, 8192.0, MEMORY_HINT),
        "MB" => (&MEMORY_UNITS, 1024.0 * 1024.0, MEMORY_HINT),
        "ms" => (&TIME_UNITS, 1.0, TIME_HINT),
        "s" => (&TIME_UNITS, 1_000.0, TIME_HINT),
        "min" => (&TIME_UNITS, 60_000.0, TIME_HINT),
        _ => return None,
    })
}

fn error(message: String, hint: Option<&str>) -> PgWireError {
    let mut info = ErrorInfo::new("ERROR".into(), "22023".into(), message);
    info.hint = hint.map(str::to_string);
    PgWireError::UserError(Box::new(info))
}

fn invalid(name: &str, value: &str, hint: Option<&str>) -> PgWireError {
    error(
        format!("invalid value for parameter \"{name}\": \"{value}\""),
        hint,
    )
}

/// C's `isspace`.
fn is_c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// `strtol(s, &end, 0)`: the value and the bytes it took, `None` for the
/// value when it overflows (`ERANGE`). `(Some(0), 0)` when nothing parsed.
fn strtol(s: &str) -> (Option<i64>, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_c_space(b[i] as char) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let (radix, mut j) = if b.len() > i + 2
        && b[i] == b'0'
        && (b[i + 1] | 0x20) == b'x'
        && (b[i + 2] as char).is_ascii_hexdigit()
    {
        (16, i + 2)
    } else if i < b.len() && b[i] == b'0' {
        (8, i)
    } else {
        (10, i)
    };
    let start = j;
    let mut value: Option<i64> = Some(0);
    while j < b.len() {
        let Some(d) = (b[j] as char).to_digit(radix) else {
            break;
        };
        value = value
            .and_then(|v| v.checked_mul(i64::from(radix)))
            .and_then(|v| {
                if neg {
                    v.checked_sub(i64::from(d))
                } else {
                    v.checked_add(i64::from(d))
                }
            });
        j += 1;
    }
    if j == start {
        return (Some(0), 0);
    }
    (value, j)
}

/// `strtod(s, &end)` for decimal text and the infinities and NaN: the value
/// and the bytes it took (`0` when nothing parsed), and whether it was out
/// of range (`ERANGE`: an overflow, or an underflow to zero).
fn strtod(s: &str) -> (f64, usize, bool) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_c_space(b[i] as char) {
        i += 1;
    }
    let num_start = i;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let rest = &s[i..];
    let lower = rest.to_ascii_lowercase();
    for word in ["infinity", "inf", "nan"] {
        if lower.starts_with(word) {
            let end = i + word.len();
            let v: f64 = s[num_start..end].parse().unwrap_or(f64::NAN);
            return (v, end, false);
        }
    }
    let digits = |mut k: usize| {
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        k
    };
    let int_end = digits(i);
    let mut end = int_end;
    let mut mantissa = int_end > i;
    if end < b.len() && b[end] == b'.' {
        let frac_end = digits(end + 1);
        mantissa |= frac_end > end + 1;
        end = frac_end;
    }
    if !mantissa {
        return (0.0, 0, false);
    }
    if end < b.len() && (b[end] | 0x20) == b'e' {
        let mut k = end + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let exp_end = digits(k);
        if exp_end > k {
            end = exp_end;
        }
    }
    let text = &s[num_start..end];
    let v: f64 = text.parse().unwrap_or(0.0);
    let nonzero = text
        .split(['e', 'E'])
        .next()
        .is_some_and(|m| m.bytes().any(|c| (b'1'..=b'9').contains(&c)));
    (v, end, v.is_infinite() || (v == 0.0 && nonzero))
}

/// `convert_to_base_unit`: `value` given in `unit` (the text after the
/// number) in the parameter's base unit, `None` for a unit it does not
/// take or anything after it. A fraction is rounded to the next smaller
/// unit (`1.5MB` is `1536kB`).
fn to_base_unit(value: f64, unit: &str, table: &[Unit], base: f64) -> Option<f64> {
    let word: String = unit
        .chars()
        .take_while(|c| !is_c_space(*c))
        .take(3)
        .collect();
    if !unit[word.len()..].chars().all(is_c_space) {
        return None;
    }
    let at = table.iter().position(|(u, _)| *u == word)?;
    let mut v = value * (table[at].1 / base);
    if let Some((_, smaller)) = table.get(at + 1) {
        let step = smaller / base;
        v = (v / step).round_ties_even() * step;
    }
    Some(v)
}

/// C's `%g`: six significant digits, the exponent form outside `1e-4 ..
/// 1e6`, trailing zeros dropped; PostgreSQL's own `snprintf` spells the
/// infinities and NaN out.
pub(crate) fn format_g(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let sci = format!("{v:.5e}");
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("an exponent");
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    } else {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        trim(&format!("{v:.decimals$}"))
    }
}

/// `_ShowOption` for an integer in its base unit: the largest unit that
/// divides it evenly, bare for zero and below.
fn show_int(v: i64, unit: &str) -> String {
    let Some((table, base, _)) = base_unit(unit).filter(|_| v > 0) else {
        return v.to_string();
    };
    for (name, size) in table {
        let mult = size / base;
        if mult <= 1.0 || v % (mult as i64) == 0 {
            return format!("{}{name}", (v as f64 / mult).round_ties_even() as i64);
        }
    }
    v.to_string()
}

/// `_ShowOption` for a real in its base unit: the largest unit it is a
/// whole number of (to within 1e-8), else the smallest.
fn show_real(v: f64, unit: &str) -> String {
    let Some((table, base, _)) = base_unit(unit).filter(|_| v > 0.0) else {
        return format_g(v);
    };
    let mut shown = (v, "");
    for (name, size) in table {
        let value = v / (size / base);
        shown = (value, name);
        if value > 0.0 && ((value.round_ties_even() / value) - 1.0).abs() <= 1e-8 {
            break;
        }
    }
    format!("{}{}", format_g(shown.0), shown.1)
}

/// `parse_bool_with_len`: a prefix of `true` / `false` / `yes` / `no`,
/// `on` / `off` (two letters at least), `1` / `0`, in any case.
fn parse_bool(value: &str) -> Option<bool> {
    let v = value.to_ascii_lowercase();
    let prefix_of = |word: &str, least: usize| v.len() >= least && word.starts_with(&v);
    match v.chars().next()? {
        't' if prefix_of("true", 1) => Some(true),
        'f' if prefix_of("false", 1) => Some(false),
        'y' if prefix_of("yes", 1) => Some(true),
        'n' if prefix_of("no", 1) => Some(false),
        'o' if prefix_of("on", 2) => Some(true),
        'o' if prefix_of("off", 2) => Some(false),
        '1' if v.len() == 1 => Some(true),
        '0' if v.len() == 1 => Some(false),
        _ => None,
    }
}

/// `value` for parameter `name` as `SHOW` would then answer it, or
/// PostgreSQL's 22023 refusal. `None` for a parameter this does not type:
/// a string, or one PostgreSQL 15 does not have.
pub(crate) fn canonical(name: &str, value: &str) -> Option<PgWireResult<String>> {
    let p = pg15_setting(name)?;
    Some(match p.vartype {
        "bool" => parse_bool(value)
            .map(|b| if b { "on" } else { "off" }.to_string())
            .ok_or_else(|| {
                error(
                    format!("parameter \"{}\" requires a Boolean value", p.name),
                    None,
                )
            }),
        "integer" => canonical_int(p, value),
        "real" => canonical_real(p, value),
        "enum" => canonical_enum(p, value),
        _ => return None,
    })
}

/// A number and, where the parameter has a unit, the text after it in that
/// unit, in the base unit.
fn with_unit(p: &Pg15Setting, value: &str, number: f64, rest: &str) -> PgWireResult<f64> {
    let rest = rest.trim_start_matches(is_c_space);
    if rest.is_empty() {
        return Ok(number);
    }
    let Some((table, base, hint)) = base_unit(p.unit) else {
        return Err(invalid(p.name, value, None));
    };
    to_base_unit(number, rest, table, base).ok_or_else(|| invalid(p.name, value, Some(hint)))
}

fn spaced_unit(unit: &str) -> String {
    if unit.is_empty() {
        String::new()
    } else {
        format!(" {unit}")
    }
}

/// `parse_int`, then the range check.
fn canonical_int(p: &Pg15Setting, value: &str) -> PgWireResult<String> {
    let (int, used) = strtol(value);
    let next = value.as_bytes().get(used).copied();
    let (number, used) = if int.is_none() || matches!(next, Some(b'.' | b'e' | b'E')) {
        let (v, used, range) = strtod(value);
        if range {
            return Err(invalid(p.name, value, None));
        }
        (v, used)
    } else {
        (int.unwrap_or(0) as f64, used)
    };
    if used == 0 || number.is_nan() {
        return Err(invalid(p.name, value, None));
    }
    let v = with_unit(p, value, number, &value[used..])?.round_ties_even();
    if v > f64::from(i32::MAX) || v < f64::from(i32::MIN) {
        return Err(invalid(p.name, value, Some("Value exceeds integer range.")));
    }
    let v = v as i64;
    let min: i64 = p.min_val.parse().unwrap_or(i64::from(i32::MIN));
    let max: i64 = p.max_val.parse().unwrap_or(i64::from(i32::MAX));
    if v < min || v > max {
        return Err(error(
            format!(
                "{v}{} is outside the valid range for parameter \"{}\" ({} .. {})",
                spaced_unit(p.unit),
                p.name,
                p.min_val,
                p.max_val
            ),
            None,
        ));
    }
    Ok(show_int_as(p, v))
}

/// The parameters whose show hooks print a file mode in octal.
const OCTAL: [&str; 3] = [
    "data_directory_mode",
    "log_file_mode",
    "unix_socket_permissions",
];

/// `_ShowOption` of `v` for `p`, or its show hook: a file mode prints in
/// octal, and a TCP setting prints the socket's value, a bare number (on
/// Linux, the exemplar build, the value set).
fn show_int_as(p: &Pg15Setting, v: i64) -> String {
    match p.name {
        n if OCTAL.contains(&n) => format!("{v:04o}"),
        "tcp_keepalives_count"
        | "tcp_keepalives_idle"
        | "tcp_keepalives_interval"
        | "tcp_user_timeout" => v.to_string(),
        _ => show_int(v, p.unit),
    }
}

/// `parse_real`, then the range check.
fn canonical_real(p: &Pg15Setting, value: &str) -> PgWireResult<String> {
    let (number, used, range) = strtod(value);
    if used == 0 || range || number.is_nan() {
        return Err(invalid(p.name, value, None));
    }
    let v = with_unit(p, value, number, &value[used..])?;
    let min: f64 = p.min_val.parse().unwrap_or(f64::MIN);
    let max: f64 = p.max_val.parse().unwrap_or(f64::MAX);
    if v < min || v > max {
        return Err(error(
            format!(
                "{}{} is outside the valid range for parameter \"{}\" ({} .. {})",
                format_g(v),
                spaced_unit(p.unit),
                p.name,
                p.min_val,
                p.max_val
            ),
            None,
        ));
    }
    Ok(show_real(v, p.unit))
}

/// `config_enum_lookup_by_name`: a visible name or a hidden spelling, in
/// any case, shown as the visible name.
fn canonical_enum(p: &Pg15Setting, value: &str) -> PgWireResult<String> {
    let lower = value.to_ascii_lowercase();
    if let Some(v) = hidden_enum_value(p.name, &lower) {
        return Ok(v.to_string());
    }
    if let Some(v) = p
        .enumvals
        .split(',')
        .find(|v| v.eq_ignore_ascii_case(value))
    {
        return Ok(v.to_string());
    }
    let available = p.enumvals.split(',').collect::<Vec<_>>().join(", ");
    Err(invalid(
        p.name,
        value,
        Some(&format!("Available values: {available}.")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(name: &str, value: &str) -> String {
        match canonical(name, value).expect("typed") {
            Ok(v) => v,
            Err(PgWireError::UserError(e)) => match e.hint {
                Some(h) => format!("ERR {} | {h}", e.message),
                None => format!("ERR {}", e.message),
            },
            Err(e) => panic!("{e:?}"),
        }
    }

    #[test]
    fn every_default_shows_as_postgres_shows_it() {
        // `_ShowOption` of each parameter's boot value is the dump's SHOW.
        for p in crate::pg15_settings::pg15_settings() {
            if !matches!(p.vartype, "integer" | "real") || p.setting.is_empty() {
                continue;
            }
            let base = if p.vartype == "integer" {
                // A file mode is dumped in octal too.
                let v = if OCTAL.contains(&p.name) {
                    i64::from_str_radix(p.setting, 8).expect("an octal mode")
                } else {
                    p.setting.parse().expect("an integer")
                };
                show_int_as(p, v)
            } else {
                show_real(p.setting.parse().expect("a real"), p.unit)
            };
            assert_eq!(base, p.show, "{}", p.name);
        }
    }

    #[test]
    fn integers_read_as_parse_int_reads_them() {
        // Each measured on PostgreSQL 15.19.
        for (value, want) in [
            ("8192", "8MB"),
            ("8193", "8193kB"),
            ("1024kB", "1MB"),
            ("1.5MB", "1536kB"),
            ("0x400", "1MB"),
            ("1e4", "10000kB"),
            ("1 MB", "1MB"),
            ("  2MB  ", "2MB"),
            ("1TB", "1TB"),
            ("2048.4", "2MB"),
            ("64", "64kB"),
        ] {
            assert_eq!(show("work_mem", value), want, "{value}");
        }
        let range = "is outside the valid range for parameter \"work_mem\" (64 .. 2147483647)";
        for (value, want) in [
            ("010", format!("ERR 8 kB {range}")),
            ("1kB", format!("ERR 1 kB {range}")),
            ("-1", format!("ERR -1 kB {range}")),
            ("1B", format!("ERR 0 kB {range}")),
            ("1.5", format!("ERR 2 kB {range}")),
            (
                "8mb",
                format!("ERR invalid value for parameter \"work_mem\": \"8mb\" | {MEMORY_HINT}"),
            ),
            (
                "4MB foo",
                format!(
                    "ERR invalid value for parameter \"work_mem\": \"4MB foo\" | {MEMORY_HINT}"
                ),
            ),
            (
                "2147483648",
                "ERR invalid value for parameter \"work_mem\": \"2147483648\" | Value exceeds integer range."
                    .to_string(),
            ),
            (
                "abc",
                "ERR invalid value for parameter \"work_mem\": \"abc\"".to_string(),
            ),
        ] {
            assert_eq!(show("work_mem", value), want, "{value}");
        }
        assert_eq!(show("temp_buffers", "800"), "6400kB");
        assert_eq!(
            show("temp_buffers", "100kB"),
            "ERR 12 8kB is outside the valid range for parameter \"temp_buffers\" (100 .. 1073741823)"
        );
        for (value, want) in [
            ("0", "0"),
            ("1000", "1s"),
            ("86400000", "1d"),
            ("1.5s", "1500ms"),
            ("5us", "0"),
            ("1500us", "2ms"),
            ("10 s", "10s"),
        ] {
            assert_eq!(show("statement_timeout", value), want, "{value}");
        }
        assert_eq!(
            show("statement_timeout", "1S"),
            format!("ERR invalid value for parameter \"statement_timeout\": \"1S\" | {TIME_HINT}")
        );
        assert_eq!(show("log_min_duration_statement", "-1"), "-1");
        assert_eq!(show("geqo_threshold", "2.5"), "2");
        assert_eq!(
            show("geqo_threshold", "5kB"),
            "ERR invalid value for parameter \"geqo_threshold\": \"5kB\""
        );
    }

    #[test]
    fn reals_read_as_parse_real_reads_them() {
        for (value, want) in [
            ("4.0", "4"),
            ("1e-10", "1e-10"),
            ("1.23456789", "1.23457"),
            ("123456789", "1.23457e+08"),
            ("1e10", "1e+10"),
            ("  2 ", "2"),
        ] {
            assert_eq!(show("random_page_cost", value), want, "{value}");
        }
        assert_eq!(
            show("random_page_cost", "inf"),
            "ERR Infinity is outside the valid range for parameter \"random_page_cost\" (0 .. 1.79769e+308)"
        );
        for value in ["x", "1e400", "nan", "5ms"] {
            assert_eq!(
                show("random_page_cost", value),
                format!("ERR invalid value for parameter \"random_page_cost\": \"{value}\"")
            );
        }
        assert_eq!(show("vacuum_cost_delay", "1.5"), "1500us");
        assert_eq!(show("vacuum_cost_delay", "0.001"), "1us");
        assert_eq!(
            show("vacuum_cost_delay", "1s"),
            "ERR 1000 ms is outside the valid range for parameter \"vacuum_cost_delay\" (0 .. 100)"
        );
    }

    #[test]
    fn booleans_and_enums_read_as_postgres_reads_them() {
        for (value, want) in [("t", "on"), ("of", "off"), ("TRUE", "on"), ("0", "off")] {
            assert_eq!(show("enable_seqscan", value), want, "{value}");
        }
        for value in ["o", "On ", " on", "maybe"] {
            assert_eq!(
                show("enable_seqscan", value),
                "ERR parameter \"enable_seqscan\" requires a Boolean value"
            );
        }
        assert_eq!(show("bytea_output", "HEX"), "hex");
        assert_eq!(
            show("bytea_output", "hex "),
            "ERR invalid value for parameter \"bytea_output\": \"hex \" | Available values: escape, hex."
        );
        assert_eq!(show("synchronous_commit", "Yes"), "on");
        assert_eq!(show("wal_compression", "on"), "pglz");
        assert_eq!(
            show("default_transaction_isolation", "Read Committed"),
            "read committed"
        );
    }
}

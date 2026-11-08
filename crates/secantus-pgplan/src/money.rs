//! The `money` type: a signed 64-bit count of CENTS, tagged so it is
//! distinct from an integer (an untagged cent count would render as `150`
//! where PostgreSQL prints `$1.50`, and would accept `money + integer`,
//! which PostgreSQL refuses).
//!
//! Everything here is PostgreSQL 16 under `lc_monetary = C`: the `$` symbol,
//! a `,` thousands separator, two fraction digits, `-$1.50` for a negative
//! (and `($1.50)` accepted on input), half-away-from-zero rounding of extra
//! fraction digits, and `bigint out of range` past `92233720368547758.07`.

use bson::{Bson, Document};

use crate::{Error, Result};

/// The tag key of a money value.
pub const MONEY_KEY: &str = "__money_cents";

/// A money value from a cent count.
pub fn money_value(cents: i64) -> Bson {
    let mut d = Document::new();
    d.insert(MONEY_KEY, Bson::Int64(cents));
    Bson::Document(d)
}

/// The cent count behind a money value, or `None` for any other value.
pub fn money_cents(v: &Bson) -> Option<i64> {
    match v {
        Bson::Document(d) if d.len() == 1 => d.get_i64(MONEY_KEY).ok(),
        _ => None,
    }
}

/// `cash_out`: `$1,234.56`, `-$0.50`.
pub fn money_text(cents: i64) -> String {
    let negative = cents < 0;
    let magnitude = cents.unsigned_abs();
    let whole = magnitude / 100;
    let frac = magnitude % 100;
    let digits = whole.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{}${grouped}.{frac:02}", if negative { "-" } else { "" })
}

fn out_of_range() -> Error {
    Error::NumericOutOfRange("bigint out of range".into())
}

/// A cent count from a decimal digit string (`-1234.565` -> `-123457`),
/// rounding a third fraction digit half away from zero, as `cash_in` and the
/// `numeric -> money` cast both do. `None` when the text is not a plain
/// decimal number.
pub fn cents_from_decimal_text(text: &str) -> Option<Result<i64>> {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // whole*100 + the first two fraction digits, rounded on the third.
    if whole.trim_start_matches('0').len() > 30 {
        return Some(Err(out_of_range()));
    }
    let whole_cents: i128 = whole
        .chars()
        .fold(0i128, |acc, c| acc * 10 + i128::from(c as u8 - b'0'));
    let mut frac_chars = frac.chars();
    let d1 = frac_chars.next().map_or(0, |c| i128::from(c as u8 - b'0'));
    let d2 = frac_chars.next().map_or(0, |c| i128::from(c as u8 - b'0'));
    let d3 = frac_chars.next().map_or(0, |c| i128::from(c as u8 - b'0'));
    if whole_cents > i128::from(i64::MAX) {
        return Some(Err(out_of_range()));
    }
    let mut total = whole_cents * 100 + d1 * 10 + d2;
    if d3 >= 5 {
        total += 1;
    }
    if negative {
        total = -total;
    }
    Some(i64::try_from(total).map_err(|_| out_of_range()))
}

/// `cash_in`: an optional `$`, thousands separators (dropped wherever they
/// appear -- `1,5` is `$15.00`), a leading `-` or enclosing parentheses for
/// a negative, and at most two significant fraction digits after rounding.
pub fn parse_money(text: &str) -> Result<i64> {
    let invalid = || Error::InvalidText(format!("invalid input syntax for type money: \"{text}\""));
    let mut s = text.trim();
    let mut negative = false;
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        negative = true;
        s = inner.trim();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.trim_start();
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest.trim_start();
    }
    if let Some(rest) = s.strip_prefix('$') {
        s = rest.trim_start();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.trim_start();
    }
    let cleaned: String = s.chars().filter(|c| *c != ',').collect();
    let cents = cents_from_decimal_text(&cleaned).ok_or_else(invalid)??;
    Ok(if negative { -cents } else { cents })
}

/// The decimal text of a money value (`1234.56`), which is what its cast to
/// `numeric` produces.
pub fn money_decimal_text(cents: i64) -> String {
    let negative = cents < 0;
    let magnitude = cents.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if negative { "-" } else { "" },
        magnitude / 100,
        magnitude % 100
    )
}

/// The exact multiplication `cents * factor` where the factor is a decimal
/// digit string, rounded half away from zero to a cent -- `cash_mul_flt8`
/// rounds through a double and `cash_mul_numeric` through a numeric, and both
/// land on the same cent for any factor a test would use.
pub fn multiply(cents: i64, factor: f64) -> Result<i64> {
    let product = (cents as f64) * factor;
    let rounded = product.round();
    if !rounded.is_finite() || rounded.abs() > i64::MAX as f64 {
        return Err(out_of_range());
    }
    Ok(rounded as i64)
}

/// `cents / divisor`, truncated toward zero the way `cash_div_int4` and
/// `cash_div_flt8` do (measured: `'5'::money / 3` is `$1.66`).
pub fn divide(cents: i64, divisor: f64) -> Result<i64> {
    if divisor == 0.0 {
        return Err(Error::DivisionByZero);
    }
    let quotient = (cents as f64) / divisor;
    if !quotient.is_finite() || quotient.abs() > i64::MAX as f64 {
        return Err(out_of_range());
    }
    Ok(quotient.trunc() as i64)
}

//! `money`: a 64-bit count of cents (PostgreSQL's `cash.c` under the `C`
//! monetary locale: `$` symbol, `,` thousands, two fraction digits). Stored
//! as the decimal it equals with two places (`Decimal128("1.50")`), the form
//! the Python server stores, so a stored value compares and sums as a number
//! and only its RENDERING (`$1.50`) and its arithmetic are money's own.

use super::*;

fn out_of_range(text: &str) -> Error {
    Error::NumericOutOfRange(format!("value \"{text}\" is out of range for type money"))
}

fn bad_input(text: &str) -> Error {
    Error::InvalidText(format!("invalid input syntax for type money: \"{text}\""))
}

/// `cash_in`: an optional sign or parentheses, an optional `$`, digits with
/// `,` anywhere, an optional fraction rounded half away from zero to cents.
pub fn parse(text: &str) -> Result<i64> {
    let t = text.trim();
    let (neg_paren, t) = match t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        Some(inner) => (true, inner.trim()),
        None => (false, t),
    };
    let mut neg = neg_paren;
    let mut rest = t;
    for _ in 0..2 {
        if let Some(r) = rest.strip_prefix('-') {
            neg = !neg;
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix('+') {
            rest = r.trim_start();
        }
        if let Some(r) = rest.strip_prefix('$') {
            rest = r.trim_start();
        }
    }
    let mut whole: i128 = 0;
    let mut frac: Vec<u8> = Vec::new();
    let mut seen_digit = false;
    let mut in_frac = false;
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '0'..='9' => {
                seen_digit = true;
                if in_frac {
                    frac.push(c as u8 - b'0');
                } else {
                    whole = whole * 10 + i128::from(c as u8 - b'0');
                    if whole > i128::from(i64::MAX) {
                        return Err(out_of_range(text));
                    }
                }
            }
            ',' if !in_frac => {}
            '.' if !in_frac => in_frac = true,
            '-' if chars.peek().is_none() => neg = !neg,
            c if c.is_whitespace() => {
                if chars.any(|c| !c.is_whitespace()) {
                    return Err(bad_input(text));
                }
                break;
            }
            _ => return Err(bad_input(text)),
        }
    }
    if !seen_digit {
        return Err(bad_input(text));
    }
    let mut cents = whole * 100
        + i128::from(*frac.first().unwrap_or(&0)) * 10
        + i128::from(*frac.get(1).unwrap_or(&0));
    if frac.get(2).is_some_and(|d| *d >= 5) {
        cents += 1;
    }
    let cents = if neg { -cents } else { cents };
    i64::try_from(cents).map_err(|_| out_of_range(text))
}

/// The stored form of `cents`.
pub fn to_bson(cents: i64) -> Bson {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    numeric::numeric_bson(&format!("{sign}{}.{:02}", abs / 100, abs % 100))
}

/// The cents a stored (or computed) money value holds.
pub fn cents_of(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i) * 100),
        Bson::Int64(i) => i.checked_mul(100),
        Bson::Double(d) => Some((d * 100.0).round() as i64),
        Bson::String(s) => parse(s).ok(),
        other => {
            let text = numeric::numeric_operand_text(other)?;
            round_numeric_text(&text)
        }
    }
}

/// A numeric's text rounded half away from zero to cents (`numeric_cash`).
fn round_numeric_text(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (w, f) = t.split_once('.').unwrap_or((t, ""));
    let mut cents: i128 = w.parse::<i128>().ok()? * 100;
    let fd: Vec<i128> = f.bytes().map(|b| i128::from(b - b'0')).collect();
    cents += fd.first().copied().unwrap_or(0) * 10 + fd.get(1).copied().unwrap_or(0);
    if fd.get(2).is_some_and(|d| *d >= 5) {
        cents += 1;
    }
    i64::try_from(if neg { -cents } else { cents }).ok()
}

/// A value cast to money.
pub fn cast(value: &Bson) -> Result<Bson> {
    match value {
        Bson::Null => Ok(Bson::Null),
        Bson::String(s) => parse(s).map(to_bson),
        // A float reaches here only as an untyped parameter's value (a typed
        // float is refused at the cast); its text is money's input.
        Bson::Double(d) => parse(&d.to_string()).map(to_bson),
        Bson::Int32(_) | Bson::Int64(_) => cents_of(value)
            .map(to_bson)
            .ok_or_else(|| out_of_range(&value_text(value))),
        other if numeric::is_numeric(other) => cents_of(other)
            .map(to_bson)
            .ok_or_else(|| out_of_range(&value_text(other))),
        other => Err(Error::CannotCoerce(format!(
            "cannot cast type {} to money",
            inferred_type(other)
        ))),
    }
}

/// `cash_out`: `$1,234.50`, `-$0.01`.
pub fn render(cents: i64) -> String {
    let abs = cents.unsigned_abs();
    let whole = (abs / 100).to_string();
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!(
        "{}${grouped}.{:02}",
        if cents < 0 { "-" } else { "" },
        abs % 100
    )
}

/// The rendering of a stored money value (its text when it is not one).
pub fn render_value(v: &Bson) -> String {
    cents_of(v).map_or_else(|| value_text(v), render)
}

fn overflow() -> Error {
    Error::NumericOutOfRange("money out of range".into())
}

/// `money op x` / `x op money`, with each side's static type. `None` when
/// neither side is money. The integer operators truncate (`cash_div_int8`);
/// a float or numeric operand rounds to even (`cash_mul_flt8`, `rint`).
pub fn arith(op: &str, l: &Bson, lt: &str, r: &Bson, rt: &str) -> Option<Result<Bson>> {
    let (lm, rm) = (lt == "money", rt == "money");
    if !(lm || rm) || !matches!(op, "+" | "-" | "*" | "/") {
        return None;
    }
    if *l == Bson::Null || *r == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    let int = |v: &Bson| match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        _ => None,
    };
    let float = |v: &Bson| -> Option<f64> {
        match v {
            Bson::Double(d) => Some(*d),
            other => numeric::numeric_operand_text(other)?.parse().ok(),
        }
    };
    Some((|| match (op, lm, rm) {
        ("+" | "-", true, true) => {
            let (a, b) = (
                cents_of(l).ok_or_else(overflow)?,
                cents_of(r).ok_or_else(overflow)?,
            );
            let c = if op == "+" {
                a.checked_add(b)
            } else {
                a.checked_sub(b)
            };
            c.map(to_bson).ok_or_else(overflow)
        }
        ("/", true, true) => {
            let (a, b) = (
                cents_of(l).ok_or_else(overflow)?,
                cents_of(r).ok_or_else(overflow)?,
            );
            if b == 0 {
                return Err(Error::DivisionByZero);
            }
            Ok(Bson::Double(a as f64 / b as f64))
        }
        ("*" | "/", true, false) | ("*", false, true) => {
            let (m, other) = if lm { (l, r) } else { (r, l) };
            let cents = cents_of(m).ok_or_else(overflow)?;
            if let Some(n) = int(other) {
                if op == "*" {
                    return cents.checked_mul(n).map(to_bson).ok_or_else(overflow);
                }
                if n == 0 {
                    return Err(Error::DivisionByZero);
                }
                return Ok(to_bson(cents / n));
            }
            let f = float(other).ok_or_else(overflow)?;
            if op == "/" && f == 0.0 {
                return Err(Error::DivisionByZero);
            }
            let x = if op == "*" {
                cents as f64 * f
            } else {
                cents as f64 / f
            };
            let x = x.round_ties_even();
            if !(x >= i64::MIN as f64 && x < i64::MAX as f64) {
                return Err(overflow());
            }
            Ok(to_bson(x as i64))
        }
        _ => Err(Error::UndefinedFunction(format!(
            "operator does not exist: {} {op} {}",
            display_type(lt),
            display_type(rt)
        ))),
    })())
}

/// `num_word` from `cash.c`: 0..=999 in words.
fn num_word(value: u64) -> String {
    const SMALL: [&str; 28] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
        "thirty",
        "forty",
        "fifty",
        "sixty",
        "seventy",
        "eighty",
        "ninety",
    ];
    let big = |i: u64| SMALL[18 + i as usize];
    let small = |i: u64| SMALL[i as usize];
    let tu = value % 100;
    if value <= 20 {
        return small(value).to_string();
    }
    if tu == 0 {
        return format!("{} hundred", small(value / 100));
    }
    if value > 99 {
        if value.is_multiple_of(10) && tu > 10 {
            format!("{} hundred {}", small(value / 100), big(tu / 10))
        } else if tu < 20 {
            format!("{} hundred and {}", small(value / 100), small(tu))
        } else {
            format!(
                "{} hundred {} {}",
                small(value / 100),
                big(tu / 10),
                small(tu % 10)
            )
        }
    } else if value.is_multiple_of(10) && tu > 10 {
        big(tu / 10).to_string()
    } else if tu < 20 {
        small(tu).to_string()
    } else {
        format!("{} {}", big(tu / 10), small(tu % 10))
    }
}

/// `cash_words`: the amount in English words.
pub fn words(cents: i64) -> String {
    let mut buf = String::new();
    if cents < 0 {
        buf.push_str("minus ");
    }
    let val = cents.unsigned_abs();
    let dollars = val / 100;
    let part = |div: u64| (val / div) % 1000;
    for (m, suffix) in [
        (part(100_000_000_000_000_000), " quadrillion "),
        (part(100_000_000_000_000), " trillion "),
        (part(100_000_000_000), " billion "),
        (part(100_000_000), " million "),
        (part(100_000), " thousand "),
    ] {
        if m != 0 {
            buf.push_str(&num_word(m));
            buf.push_str(suffix);
        }
    }
    let m1 = part(100);
    if m1 != 0 {
        buf.push_str(&num_word(m1));
    }
    if dollars == 0 {
        buf.push_str("zero");
    }
    buf.push_str(if dollars == 1 {
        " dollar and "
    } else {
        " dollars and "
    });
    let m0 = val % 100;
    buf.push_str(&num_word(m0));
    buf.push_str(if m0 == 1 { " cent" } else { " cents" });
    let mut chars = buf.chars();
    match chars.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + chars.as_str(),
        None => buf,
    }
}

/// The type `money op x` yields, or `None` when neither side is money.
pub fn arith_type(op: &str, lt: &str, rt: &str) -> Option<&'static str> {
    if lt != "money" && rt != "money" {
        return None;
    }
    Some(match op {
        "/" if lt == "money" && rt == "money" => "float8",
        "+" | "-" | "*" | "/" => "money",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders_like_cash_in_out() {
        assert_eq!(parse("12.345").unwrap(), 1235);
        assert_eq!(parse("-12.345").unwrap(), -1235);
        assert_eq!(parse("(5)").unwrap(), -500);
        assert_eq!(parse("$-1,0.1").unwrap(), -1010);
        assert_eq!(parse("92233720368547758.07").unwrap(), i64::MAX);
        assert!(parse("92233720368547758.08").is_err());
        assert!(parse("abc").is_err());
        assert!(parse("1e3").is_err());
        assert_eq!(render(123_450), "$1,234.50");
        assert_eq!(render(-1), "-$0.01");
        assert_eq!(render(i64::MIN), "-$92,233,720,368,547,758.08");
    }
}

//! PostgreSQL `numeric`: arbitrary precision, stored in BSON.
//!
//! A `numeric` carries its display scale as part of the value -- `1.50` is not
//! `1.5` -- and has no upper bound on its digits (131072 before the point,
//! 16383 after). BSON `Decimal128` keeps the scale the same way but holds only
//! 34 significant digits, so a value that fits Decimal128 EXACTLY -- digits AND
//! scale -- is stored as one, and every other value is stored as a
//! marker-key document (the convention every other wider-than-BSON type here
//! uses):
//!
//! ```text
//! { "__numeric": "<PostgreSQL canonical text>", "__numkey": "<sortable key>" }
//! ```
//!
//! `__numeric` is the value as PostgreSQL renders it (plain, never exponent
//! notation, trailing zeros kept). `__numkey` is a string whose BYTEWISE order
//! is numeric order, so a WHERE clause can be lowered to an exact MQL string
//! comparison on it -- the storage layer's document sort would otherwise put
//! every wide value in the document bracket, above every Decimal128, which is
//! wrong rows rather than slow rows.
//!
//! Which form a value takes is decided by `numeric_bson`, and every consumer
//! accepts both through `numeric_text` -- the same column can hold either.

use std::cmp::Ordering;
use std::str::FromStr;

use bson::{doc, Bson, Decimal128, Document};
use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::{Error, Result};

/// Marker key carrying the canonical text of a numeric too wide for Decimal128.
pub const WIDE_NUMERIC_KEY: &str = "__numeric";
/// Marker key carrying the byte-sortable key of a wide numeric.
pub const WIDE_NUMERIC_SORT_KEY: &str = "__numkey";

/// PostgreSQL's own limits, probed on 16: `1e131071` is the widest integer
/// (131072 digits) and `1e-16383` the smallest scale that parse; one more of
/// either is "value overflows numeric format".
const MAX_INT_DIGITS: usize = 131072;
const MAX_SCALE: usize = 16383;

/// Decimal128 holds a 34-digit coefficient times `10^exp`, `exp` in this range.
const DECIMAL128_DIGITS: usize = 34;
const DECIMAL128_MIN_EXP: i64 = -6176;
const DECIMAL128_MAX_EXP: i64 = 6111;

/// A finite canonical numeric, split into its parts. `int` has no leading
/// zeros (empty for a pure fraction); `frac` is the display scale, zeros kept.
struct Parts<'a> {
    neg: bool,
    int: &'a str,
    frac: &'a str,
}

fn split_canonical(text: &str) -> Option<Parts<'_>> {
    let (neg, body) = match text.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text),
    };
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let int = int.trim_start_matches('0');
    Some(Parts { neg, int, frac })
}

fn is_zero_digits(p: &Parts<'_>) -> bool {
    p.int.is_empty() && p.frac.bytes().all(|b| b == b'0')
}

/// The canonical PostgreSQL text of a numeric literal, or the input's error.
///
/// Rules, all probed on PostgreSQL 16: exponent notation is expanded
/// (`1.5e20` is `150000000000000000000`); the display scale of an exponent
/// form is `max(0, fraction digits - exponent)` (`1.1e-5` is `0.000011`,
/// `100e-1` is `10.0`, `1.50e1` is `15.0`); leading integer zeros go
/// (`0001.10` is `1.10`) while trailing fraction zeros stay; there is no
/// negative zero (`-0.0` is `0.0`); `1_000` and `'1_000.5'` are accepted;
/// surrounding whitespace is ignored; `NaN`, `Infinity` and `-Infinity` are
/// case-insensitive and render in that spelling.
pub fn canonical_numeric_text(input: &str) -> Result<String> {
    let t = input.trim();
    let invalid = || Error::InvalidText(format!("invalid input syntax for type numeric: \"{t}\""));
    let overflow = || Error::NumericOutOfRange("value overflows numeric format".to_string());
    match t.to_ascii_lowercase().as_str() {
        "nan" | "+nan" | "-nan" => return Ok("NaN".to_string()),
        "inf" | "+inf" | "infinity" | "+infinity" => return Ok("Infinity".to_string()),
        "-inf" | "-infinity" => return Ok("-Infinity".to_string()),
        _ => {}
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    let exp: i64 = match exp {
        Some(e) => {
            let e = e.strip_prefix('+').unwrap_or(e);
            let digits = e.strip_prefix('-').unwrap_or(e);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            // Any exponent this large overflows whatever the mantissa is.
            e.parse::<i64>().map_err(|_| overflow())?
        }
        None => 0,
    };
    let (int_raw, frac_raw) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    // Underscores are digit-group separators: between digits only.
    let strip_groups = |s: &str| -> Result<String> {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'0'..=b'9' => out.push(b as char),
                b'_' if i > 0
                    && i + 1 < bytes.len()
                    && bytes[i - 1].is_ascii_digit()
                    && bytes[i + 1].is_ascii_digit() => {}
                _ => return Err(invalid()),
            }
        }
        Ok(out)
    };
    let int_raw = strip_groups(int_raw)?;
    let frac_raw = strip_groups(frac_raw)?;
    if int_raw.is_empty() && frac_raw.is_empty() {
        return Err(invalid());
    }
    if exp.unsigned_abs() > (MAX_INT_DIGITS + MAX_SCALE) as u64 {
        return Err(overflow());
    }
    let mut digits = format!("{int_raw}{frac_raw}");
    let mut point = int_raw.len() as i64 + exp;
    if point <= 0 {
        digits = format!("{}{digits}", "0".repeat((1 - point) as usize));
        point = 1;
    }
    while (digits.len() as i64) < point {
        digits.push('0');
    }
    let (int, frac) = digits.split_at(point as usize);
    let int = int.trim_start_matches('0');
    if int.len() > MAX_INT_DIGITS || frac.len() > MAX_SCALE {
        return Err(overflow());
    }
    let zero = int.is_empty() && frac.bytes().all(|b| b == b'0');
    let sign = if neg && !zero { "-" } else { "" };
    let int = if int.is_empty() { "0" } else { int };
    Ok(if frac.is_empty() {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{frac}")
    })
}

/// A `numeric` rendered the way PostgreSQL renders one: PLAIN, never in
/// exponent notation.
///
/// `Decimal128`'s own rendering keeps the scale (`1.50`, not `1.5`), which is
/// part of a numeric's value and must survive -- but it also falls back to
/// `E` notation for large and small magnitudes, and PostgreSQL never does:
/// `1.5e20::numeric` is `150000000000000000000` there and was `1.5E+20` here,
/// in the row, in a `::text` cast and inside an array. A value the client
/// cannot tell from the right one only by luck of `Decimal` comparing equal.
///
/// The non-finite renderings (`NaN`, `Infinity`) carry no exponent and pass
/// through untouched.
pub fn plain_numeric_text(text: &str) -> String {
    let Some(epos) = text.find(['e', 'E']) else {
        return text.to_string();
    };
    let (mantissa, exp) = text.split_at(epos);
    let Ok(exp) = exp[1..].parse::<i32>() else {
        return text.to_string();
    };
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (mantissa.to_string(), String::new()),
    };
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return text.to_string();
    }
    let mut digits = format!("{int_part}{frac_part}");
    // Where the point sits, counted from the left of the digit string.
    let mut point = int_part.len() as i32 + exp;
    if point <= 0 {
        // Padding on the left puts the point just after the digits added,
        // which is position 1 whatever it was before.
        digits = format!("{}{digits}", "0".repeat((1 - point) as usize));
        point = 1;
    }
    while (digits.len() as i32) < point {
        digits.push('0');
    }
    let (i, f) = digits.split_at(point as usize);
    if f.is_empty() {
        // A zero mantissa with a positive exponent (`0.00e3` is `0E+1`)
        // is plain `0`, not one zero per power of ten: PostgreSQL gives a
        // zero no display scale it did not write.
        if i.bytes().all(|b| b == b'0') {
            return format!("{sign}0");
        }
        format!("{sign}{i}")
    } else {
        format!("{sign}{i}.{f}")
    }
}

/// The Decimal128 that holds this canonical text EXACTLY -- digits and display
/// scale both -- or `None` when no Decimal128 does.
///
/// `Decimal128::from_str` is not the judge: it silently drops a trailing zero
/// to make a 35-digit `100000000000000000.00000000000000000` fit, and that
/// zero is part of the value (`::text` shows it, and psycopg's `Decimal`
/// compares it). The constructed value is rendered back and must match.
fn exact_decimal128(canonical: &str) -> Option<Decimal128> {
    let p = split_canonical(canonical)?;
    let joined = format!("{}{}", p.int, p.frac);
    let mut coefficient = joined.trim_start_matches('0');
    let mut exp = -(p.frac.len() as i64);
    if p.frac.is_empty() {
        let trimmed = coefficient.trim_end_matches('0');
        exp += (coefficient.len() - trimmed.len()) as i64;
        coefficient = trimmed;
    }
    if coefficient.is_empty() {
        coefficient = "0";
    }
    if coefficient.len() > DECIMAL128_DIGITS
        || !(DECIMAL128_MIN_EXP..=DECIMAL128_MAX_EXP).contains(&exp)
    {
        return None;
    }
    let sign = if p.neg { "-" } else { "" };
    let d = Decimal128::from_str(&format!("{sign}{coefficient}E{exp}")).ok()?;
    (plain_numeric_text(&d.to_string()) == canonical).then_some(d)
}

/// The BSON form of a canonical numeric text: a Decimal128 when one holds it
/// exactly, else the wide-numeric document.
pub fn numeric_bson(canonical: &str) -> Bson {
    if let Ok(d) = Decimal128::from_str(canonical) {
        if !canonical.bytes().any(|b| b.is_ascii_digit()) {
            // NaN / Infinity: Decimal128 has them, and they never widen.
            return Bson::Decimal128(d);
        }
    }
    match exact_decimal128(canonical) {
        Some(d) => Bson::Decimal128(d),
        None => Bson::Document(doc! {
            WIDE_NUMERIC_KEY: canonical,
            WIDE_NUMERIC_SORT_KEY: numeric_sort_key(canonical),
        }),
    }
}

/// Parse a `numeric` literal into its stored form.
pub fn parse_numeric(text: &str) -> Result<Bson> {
    Ok(numeric_bson(&canonical_numeric_text(text)?))
}

/// Whether a stored value is a numeric of either width.
pub fn is_numeric(v: &Bson) -> bool {
    match v {
        Bson::Decimal128(_) => true,
        Bson::Document(d) => d.contains_key(WIDE_NUMERIC_KEY),
        _ => false,
    }
}

/// Whether a stored value is the wide-numeric document.
pub fn is_wide_numeric(v: &Bson) -> bool {
    matches!(v, Bson::Document(d) if d.contains_key(WIDE_NUMERIC_KEY))
}

/// The canonical PostgreSQL text of a stored numeric of either width.
pub fn numeric_text(v: &Bson) -> Option<String> {
    match v {
        Bson::Decimal128(d) => Some(plain_numeric_text(&d.to_string())),
        Bson::Document(d) => match d.get(WIDE_NUMERIC_KEY) {
            Some(Bson::String(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The canonical text of anything PostgreSQL would treat as a numeric operand:
/// a numeric of either width, or an integer.
pub fn numeric_operand_text(v: &Bson) -> Option<String> {
    match v {
        Bson::Int32(i) => Some(i.to_string()),
        Bson::Int64(i) => Some(i.to_string()),
        other => numeric_text(other),
    }
}

/// A string whose BYTEWISE order is the numeric order of the canonical texts
/// it is built from, `NaN` above everything as PostgreSQL has it.
///
/// Layout: a class byte (`0` -Infinity, `1` negative, `2` zero, `3` positive,
/// `4` Infinity, `5` NaN); then for a non-zero finite value the decimal
/// exponent `E` of `0.d1d2… × 10^E` as a fixed-width offset decimal, then the
/// significant digits with trailing zeros removed. A negative value's exponent
/// and digits are nines-complemented and the digits get a `~` terminator, so
/// a longer (larger-magnitude) negative sorts FIRST.
pub fn numeric_sort_key(canonical: &str) -> String {
    match canonical {
        "NaN" => return "5".to_string(),
        "Infinity" => return "4".to_string(),
        "-Infinity" => return "0".to_string(),
        _ => {}
    }
    let Some(p) = split_canonical(canonical) else {
        return "5".to_string();
    };
    if is_zero_digits(&p) {
        return "2".to_string();
    }
    let all = format!("{}{}", p.int, p.frac);
    let leading_zeros = all.bytes().take_while(|b| *b == b'0').count();
    let exponent = p.int.len() as i64 - leading_zeros as i64;
    let significant = all[leading_zeros..].trim_end_matches('0');
    let exp_field = format!("{:07}", exponent + 1_000_000);
    if p.neg {
        let complement = |s: &str| -> String {
            s.bytes()
                .map(|b| (b'0' + (9 - (b - b'0'))) as char)
                .collect()
        };
        format!("1{}{}~", complement(&exp_field), complement(significant))
    } else {
        format!("3{exp_field}{significant}")
    }
}

/// Where a numeric constant sits relative to the Decimal128 number line.
#[derive(Debug, Clone, PartialEq)]
pub enum Bracket {
    /// The constant IS this Decimal128.
    Exact(Decimal128),
    /// No Decimal128 equals the constant; these are the nearest below and above.
    Between(Decimal128, Decimal128),
}

fn decimal128(text: &str) -> Decimal128 {
    Decimal128::from_str(text).expect("a Decimal128 built from a checked coefficient")
}

/// The Decimal128 bracket of a canonical numeric: a Decimal128 of the same
/// VALUE when one exists (display scale aside -- `1.000…0` with forty zeros is
/// the value `1`), else the adjacent Decimal128s on either side. Every
/// Decimal128 stored in a column compares with the constant exactly as it
/// compares with the bracket, which is what lets a wide constant be lowered
/// to MQL.
pub fn decimal128_bracket(canonical: &str) -> Option<Bracket> {
    if non_finite_class(canonical).is_some() {
        return Some(Bracket::Exact(decimal128(canonical)));
    }
    let p = split_canonical(canonical)?;
    if is_zero_digits(&p) {
        return Some(Bracket::Exact(decimal128("0")));
    }
    let all = format!("{}{}", p.int, p.frac);
    let leading_zeros = all.bytes().take_while(|b| *b == b'0').count();
    let exponent = p.int.len() as i64 - leading_zeros as i64;
    let significant = all[leading_zeros..].trim_end_matches('0');
    let sign = if p.neg { "-" } else { "" };
    if significant.len() <= DECIMAL128_DIGITS {
        let tail_exp = exponent - significant.len() as i64;
        if let Ok(d) = Decimal128::from_str(&format!("{sign}{significant}E{tail_exp}")) {
            return Some(Bracket::Exact(d));
        }
    }
    // Magnitudes: truncated to the digits Decimal128 can hold HERE, and one
    // unit above that. That is 34 digits -- unless the value sits so close to
    // Decimal128's exponent floor that 34 digits would put the last one below
    // 10^-6176, in which case only the digits down to 10^-6176 fit. (Truncating
    // to 34 regardless made `mk` fail and fall back to `(0, 1E-6176)`, a
    // bracket that does not CONTAIN a value like 1.23…E-6150, so a range
    // filter on it selected wrong Decimal128 rows.) With no digit left the
    // value is below 1E-6176, and that fallback is then exactly right.
    let mut take = significant.len().min(DECIMAL128_DIGITS);
    if exponent - (take as i64) < DECIMAL128_MIN_EXP {
        take = (exponent - DECIMAL128_MIN_EXP).clamp(0, take as i64) as usize;
    }
    let head = &significant[..take];
    let tail_exp = exponent - take as i64;
    let lo: BigInt = if head.is_empty() {
        BigInt::zero()
    } else {
        head.parse().ok()?
    };
    let hi = &lo + BigInt::one();
    let mk = |n: &BigInt| Decimal128::from_str(&format!("{n}E{tail_exp}")).ok();
    let (below, above) = match (mk(&lo), mk(&hi)) {
        (Some(lo), Some(hi)) => (lo, hi),
        _ if exponent > 0 => (
            decimal128("9999999999999999999999999999999999E+6111"),
            decimal128("Infinity"),
        ),
        _ => (decimal128("0"), decimal128("1E-6176")),
    };
    Some(if p.neg {
        let neg = |d: Decimal128| -> Decimal128 {
            let t = d.to_string();
            decimal128(&match t.strip_prefix('-') {
                Some(r) => r.to_string(),
                None => format!("-{t}"),
            })
        };
        Bracket::Between(neg(above), neg(below))
    } else {
        Bracket::Between(below, above)
    })
}

/// `field <mql_op> value` as an MQL filter that is exact for a column holding
/// numerics of either width.
///
/// The narrow arm compares the stored Decimal128 against the constant's
/// bracket; the wide arm compares the stored `__numkey` string against the
/// constant's own key. A missing `__numkey` (every Decimal128 row) never
/// satisfies a range or equality on it, and always satisfies `$ne` -- which is
/// exactly the arm those rows should take.
///
/// NaN takes PostgreSQL's place in the order -- equal to itself and ABOVE
/// every number, infinity included (probed on 16) -- where MQL's range
/// operators exclude it. So a NaN constant is lowered by hand, and a `$gt` /
/// `$gte` against any other constant picks up the NaN rows as a third arm.
/// (A NaN is always a Decimal128; the wide form holds finite values only.)
pub fn numeric_filter(field: &str, mql_op: &str, value: &Bson) -> Option<Document> {
    let canonical = numeric_operand_text(value)?;
    let key = numeric_sort_key(&canonical);
    let wide_field = format!("{field}.{WIDE_NUMERIC_SORT_KEY}");
    let nan = decimal128("NaN");
    if canonical == "NaN" {
        return match mql_op {
            "$eq" | "$gte" => Some(doc! { field: nan }),
            "$ne" => Some(doc! { "$and": [
                { field: { "$ne": nan } },
                { field: { "$ne": Bson::Null } },
            ]}),
            "$gt" => Some(doc! { field: { "$in": [] } }),
            "$lt" => Some(doc! { "$and": [
                { field: { "$ne": nan } },
                { field: { "$ne": Bson::Null } },
            ]}),
            "$lte" => Some(doc! { field: { "$ne": Bson::Null } }),
            _ => None,
        };
    }
    let (exact, lo, hi) = match decimal128_bracket(&canonical)? {
        Bracket::Exact(d) => (true, d, d),
        Bracket::Between(lo, hi) => (false, lo, hi),
    };
    if mql_op == "$ne" {
        let mut arms = Vec::new();
        if exact {
            arms.push(doc! { field: { "$ne": lo } });
        }
        arms.push(doc! { &wide_field: { "$ne": &key } });
        arms.push(doc! { field: { "$ne": Bson::Null } });
        return Some(doc! { "$and": arms });
    }
    let narrow = match mql_op {
        "$eq" => exact.then(|| doc! { field: lo }),
        "$gt" => Some(if exact {
            doc! { field: { "$gt": lo } }
        } else {
            doc! { field: { "$gte": hi } }
        }),
        "$gte" => Some(doc! { field: { "$gte": if exact { lo } else { hi } } }),
        "$lt" => Some(if exact {
            doc! { field: { "$lt": lo } }
        } else {
            doc! { field: { "$lte": lo } }
        }),
        "$lte" => Some(doc! { field: { "$lte": lo } }),
        _ => return None,
    };
    let wide = doc! { wide_field: { mql_op: key } };
    let mut arms = Vec::new();
    arms.extend(narrow);
    arms.push(wide);
    if matches!(mql_op, "$gt" | "$gte") {
        arms.push(doc! { field: nan });
    }
    Some(if arms.len() == 1 {
        arms.remove(0)
    } else {
        doc! { "$or": arms }
    })
}

/// Compare two canonical numeric texts EXACTLY, digit by digit.
///
/// A `numeric` carries any number of digits and an `f64` holds 15, so routing
/// a comparison through a float can report two different numbers as equal.
/// Scale is not part of equality -- `1.50 = 1.5` is true -- so trailing zeros
/// are trimmed before comparing.
///
/// PostgreSQL gives NaN a place in a TOTAL order, unlike IEEE: NaN equals
/// itself and sorts ABOVE every number, infinity included. Probed on PG 14.
pub fn compare_decimal_text(a: &str, b: &str) -> Option<Ordering> {
    let rank = |t: &str| -> Option<i32> {
        let u = t.trim().to_ascii_lowercase();
        match u.as_str() {
            "nan" => Some(2),
            "infinity" | "inf" | "+infinity" | "+inf" => Some(1),
            "-infinity" | "-inf" => Some(-1),
            _ => None,
        }
    };
    match (rank(a), rank(b)) {
        (Some(x), Some(y)) => return Some(x.cmp(&y)),
        (Some(x), None) => {
            return Some(if x > 0 {
                Ordering::Greater
            } else {
                Ordering::Less
            })
        }
        (None, Some(y)) => {
            return Some(if y > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            })
        }
        (None, None) => {}
    }
    let split = |t: &str| -> Option<(bool, String, String)> {
        let t = t.trim();
        let (neg, body) = match t.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        let (i, f) = body.split_once('.').unwrap_or((body, ""));
        // Leading zeros in the integer part and trailing zeros in the fraction
        // change neither the value nor the ordering.
        let int = i.trim_start_matches('0').to_string();
        let frac = f.trim_end_matches('0').to_string();
        Some((neg, int, frac))
    };
    let (an, ai, af) = split(a)?;
    let (bn, bi, bf) = split(b)?;
    let a_zero = ai.is_empty() && af.is_empty();
    let b_zero = bi.is_empty() && bf.is_empty();
    // Negative zero is zero.
    let an = an && !a_zero;
    let bn = bn && !b_zero;
    if an != bn {
        return Some(if an {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let magnitude = ai
        .len()
        .cmp(&bi.len())
        .then_with(|| ai.cmp(&bi))
        .then_with(|| {
            let n = af.len().max(bf.len());
            let pad = |f: &str| format!("{f:0<width$}", width = n);
            pad(&af).cmp(&pad(&bf))
        });
    Some(if an { magnitude.reverse() } else { magnitude })
}

/// A finite decimal as an exact (unscaled value, scale) pair.
///
/// PostgreSQL's `numeric` arithmetic is EXACT and carries a defined result
/// scale, so it cannot go through an `f64`: `0.1 + 0.2` is `0.3`, not
/// `0.30000000000000004`, and an operand can have more digits than a float --
/// or a Decimal128 -- can hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dec {
    unscaled: BigInt,
    scale: u32,
}

impl Dec {
    pub fn parse(text: &str) -> Option<Dec> {
        let canonical = canonical_numeric_text(text).ok()?;
        let p = split_canonical(&canonical)?;
        let digits = format!("{}{}", if p.int.is_empty() { "0" } else { p.int }, p.frac);
        let unscaled: BigInt = digits.parse().ok()?;
        Some(Dec {
            unscaled: if p.neg { -unscaled } else { unscaled },
            scale: u32::try_from(p.frac.len()).ok()?,
        })
    }

    /// The canonical text.
    pub fn render(&self) -> String {
        if self.scale == 0 {
            return self.unscaled.to_string();
        }
        let neg = self.unscaled.is_negative();
        let digits = self.unscaled.abs().to_string();
        let scale = self.scale as usize;
        let padded = if digits.len() <= scale {
            format!("{}{}", "0".repeat(scale - digits.len() + 1), digits)
        } else {
            digits
        };
        let split = padded.len() - scale;
        format!(
            "{}{}.{}",
            if neg { "-" } else { "" },
            &padded[..split],
            &padded[split..]
        )
    }

    pub fn is_zero(&self) -> bool {
        self.unscaled.is_zero()
    }

    fn lift(&self, scale: u32) -> BigInt {
        &self.unscaled * BigInt::from(10u32).pow(scale - self.scale)
    }

    /// PostgreSQL's `weight` (base-10000 position of the leading group) and
    /// that group's value, the two inputs of its division-scale rule.
    fn weight_and_first_group(&self) -> (i64, i64) {
        let text = Dec {
            unscaled: self.unscaled.abs(),
            scale: self.scale,
        }
        .render();
        let (int, frac) = text.split_once('.').unwrap_or((text.as_str(), ""));
        let int = int.trim_start_matches('0');
        if !int.is_empty() {
            let weight = (int.len() as i64 - 1) / 4;
            let lead = int.len() - (weight as usize) * 4;
            return (weight, int[..lead].parse().unwrap_or(0));
        }
        let bytes = frac.as_bytes();
        let mut k = 0usize;
        while k * 4 < bytes.len() {
            let group = &frac[k * 4..(k * 4 + 4).min(bytes.len())];
            let value: i64 = format!("{group:0<4}").parse().unwrap_or(0);
            if value != 0 {
                return (-(k as i64) - 1, value);
            }
            k += 1;
        }
        (0, 0)
    }
}

/// The `(precision, scale)` a `numeric(p, s)` typmod packs, or `None` for an
/// unconstrained `numeric`. PostgreSQL 15+ keeps a SIGNED scale in the low
/// 11 bits (`numeric(5, -2)` is legal), which is unpacked here.
pub fn typmod_precision_scale(typmod: i32) -> Option<(i32, i32)> {
    if typmod < 4 {
        return None;
    }
    let packed = typmod - 4;
    let precision = (packed >> 16) & 0xFFFF;
    let mut scale = packed & 0x7FF;
    if scale >= 0x400 {
        scale -= 0x800;
    }
    Some((precision, scale))
}

/// PostgreSQL's `apply_typmod`: a numeric held to a declared `(p, s)` is
/// rounded half away from zero to `s` places and refused (22003) when more
/// than `p - s` digits remain before the point. Measured on 16.15:
/// `1.235::numeric(5,2)` is `1.24`, `'1.999'::numeric(5,2)` is `2.00`,
/// `1e5::numeric(5,-2)` is `100000`, `12345::numeric(5,2)` overflows, `NaN`
/// passes untouched, and an infinity is its own 22003.
pub fn apply_typmod(value: Bson, typmod: i32) -> Result<Bson> {
    let Some((precision, scale)) = typmod_precision_scale(typmod) else {
        return Ok(value);
    };
    let Some(text) = numeric_text(&value) else {
        return Ok(value);
    };
    let overflow = |detail: String| {
        Error::NumericOutOfRange(format!("numeric field overflow\nDetail: {detail}"))
    };
    if text == "NaN" {
        return Ok(value);
    }
    if text.ends_with("Infinity") {
        return Err(overflow(format!(
            "A field with precision {precision}, scale {scale} cannot hold an infinite value."
        )));
    }
    let Some(dec) = Dec::parse(&text) else {
        return Ok(value);
    };
    let rounded = if scale >= 0 {
        dec.round_to(scale as u32)
    } else {
        // A negative scale rounds to a power of ten above the point and
        // renders with that many trailing zeros.
        let unit = BigInt::from(10u32).pow((-scale) as u32);
        let lifted = dec.round_to(0);
        let q = round_half_away(&lifted.unscaled.abs(), &unit) * &unit;
        Dec {
            unscaled: if lifted.is_negative() { -q } else { q },
            scale: 0,
        }
    };
    let rendered = rounded.render();
    let int_digits = rendered
        .trim_start_matches('-')
        .split('.')
        .next()
        .unwrap_or("")
        .trim_start_matches('0')
        .len();
    if int_digits as i32 > precision - scale {
        return Err(overflow(format!(
            "A field with precision {precision}, scale {scale} must round to an absolute value less than 10^{}.",
            precision - scale
        )));
    }
    Ok(numeric_bson(&rendered))
}

fn round_half_away(num: &BigInt, den: &BigInt) -> BigInt {
    // Both non-negative.
    let (q, r) = (num / den, num % den);
    if &r * BigInt::from(2u32) >= *den {
        q + BigInt::one()
    } else {
        q
    }
}

/// PostgreSQL's `select_div_scale`: the result scale of `a / b`, from the
/// operands' weights and leading digit groups. Probed: `1.50/3` has 20 places,
/// `10.0/4` has 16, `1000000/3.0` has 12.
fn div_scale(a: &Dec, b: &Dec) -> u32 {
    let (w1, f1) = a.weight_and_first_group();
    let (w2, f2) = b.weight_and_first_group();
    let mut qweight = w1 - w2;
    if f1 <= f2 {
        qweight -= 1;
    }
    let rscale = (16 - qweight * 4)
        .max(i64::from(a.scale))
        .max(i64::from(b.scale))
        .clamp(0, 1000);
    rscale as u32
}

fn non_finite_class(canonical: &str) -> Option<i8> {
    match canonical {
        "NaN" => Some(0),
        "Infinity" => Some(1),
        "-Infinity" => Some(-1),
        _ => None,
    }
}

/// Exact `+`, `-`, `*` and `/` on numerics, with PostgreSQL's result scales:
/// addition and subtraction take `max(s1, s2)`, multiplication takes
/// `s1 + s2`, division takes `select_div_scale` and rounds half away from
/// zero. All measured -- `1.50 + 1.5` is `3.00`, `1.50 * 1.50` is `2.2500`,
/// `2::numeric / 7` is `0.28571428571428571429`.
///
/// `None` when either operand is not a numeric text; `Some(Err)` for a
/// division by zero.
pub fn decimal_arith(op: &str, a: &str, b: &str) -> Option<Result<Bson>> {
    let (ca, cb) = (
        canonical_numeric_text(a).ok()?,
        canonical_numeric_text(b).ok()?,
    );
    if let Some(text) = non_finite_arith(op, &ca, &cb)? {
        return Some(text.map(|t| numeric_bson(&t)));
    }
    let (x, y) = (Dec::parse(&ca)?, Dec::parse(&cb)?);
    let out = match op {
        "+" | "-" => {
            let scale = x.scale.max(y.scale);
            let (xa, ya) = (x.lift(scale), y.lift(scale));
            Dec {
                unscaled: if op == "+" { xa + ya } else { xa - ya },
                scale,
            }
        }
        "*" => Dec {
            unscaled: &x.unscaled * &y.unscaled,
            scale: x.scale + y.scale,
        },
        // `%` keeps the dividend's sign and the wider scale (`-7.5 % 2` is
        // `-1.5`, `1.50 % 1` is `0.50`, measured 16).
        "%" => {
            if y.is_zero() {
                return Some(Err(Error::DivisionByZero));
            }
            let scale = x.scale.max(y.scale);
            let (xa, ya) = (x.lift(scale), y.lift(scale));
            Dec {
                unscaled: xa % ya,
                scale,
            }
        }
        "/" => {
            if y.is_zero() {
                return Some(Err(Error::DivisionByZero));
            }
            let rscale = div_scale(&x, &y);
            let num = x.unscaled.abs() * BigInt::from(10u32).pow(y.scale + rscale);
            let den = y.unscaled.abs() * BigInt::from(10u32).pow(x.scale);
            let q = round_half_away(&num, &den);
            let negative = x.unscaled.is_negative() != y.unscaled.is_negative();
            Dec {
                unscaled: if negative { -q } else { q },
                scale: rscale,
            }
        }
        _ => return None,
    };
    let text = out.render();
    if canonical_numeric_text(&text).is_err() {
        return Some(Err(Error::NumericOutOfRange(
            "value overflows numeric format".to_string(),
        )));
    }
    Some(Ok(numeric_bson(&text)))
}

/// Arithmetic where either side is NaN or infinite (PostgreSQL 16 rules):
/// `Some(Some(text))` for a defined answer, `Some(Err)` for an error,
/// `Some(None)` when both sides are finite, `None` for an unknown operator.
#[allow(clippy::type_complexity)]
fn non_finite_arith(op: &str, a: &str, b: &str) -> Option<Option<Result<String>>> {
    let (ca, cb) = (non_finite_class(a), non_finite_class(b));
    if ca.is_none() && cb.is_none() {
        return match op {
            "+" | "-" | "*" | "/" | "%" => Some(None),
            _ => None,
        };
    }
    if ca == Some(0) || cb == Some(0) {
        return Some(Some(Ok("NaN".to_string())));
    }
    let sign_of = |t: &str, class: Option<i8>| -> i8 {
        match class {
            Some(c) => c,
            None => match Dec::parse(t) {
                Some(d) if d.is_zero() => 0,
                Some(d) if d.unscaled.is_negative() => -1,
                Some(_) => 1,
                None => 0,
            },
        }
    };
    let inf = |s: i8| {
        Ok(if s < 0 {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        })
    };
    let (sa, sb) = (sign_of(a, ca), sign_of(b, cb));
    let out = match op {
        "+" => match (ca, cb) {
            (Some(x), Some(y)) if x != y => Ok("NaN".to_string()),
            (Some(x), _) => inf(x),
            (_, Some(y)) => inf(y),
            _ => unreachable!(),
        },
        "-" => match (ca, cb) {
            (Some(x), Some(y)) if x == y => Ok("NaN".to_string()),
            (Some(x), _) => inf(x),
            (_, Some(y)) => inf(-y),
            _ => unreachable!(),
        },
        "*" => {
            if sa == 0 || sb == 0 {
                Ok("NaN".to_string())
            } else {
                inf(sa * sb)
            }
        }
        "/" => match (ca, cb) {
            (Some(_), Some(_)) => Ok("NaN".to_string()),
            (Some(_), None) => {
                if sb == 0 {
                    Err(Error::DivisionByZero)
                } else {
                    inf(sa * sb)
                }
            }
            (None, Some(_)) => Ok("0".to_string()),
            _ => unreachable!(),
        },
        // `Infinity % x` is NaN; `x % Infinity` is x (measured 16).
        "%" => match (ca, cb) {
            (Some(_), _) => Ok("NaN".to_string()),
            (None, Some(_)) => Ok(a.to_string()),
            _ => unreachable!(),
        },
        _ => return None,
    };
    Some(Some(out))
}

/// Unary minus over a numeric text. `NaN` is its own negation; `Infinity` and
/// `-Infinity` swap; zero stays `0` (never `-0`); anything else flips its
/// sign. The scale is untouched (`-'1.50'::numeric` is `-1.50`).
pub fn negate_numeric_text(text: &str) -> Result<Bson> {
    let canonical = canonical_numeric_text(text)
        .map_err(|_| Error::Parse(format!("cannot negate numeric {text}")))?;
    let out = match canonical.strip_prefix('-') {
        Some(rest) => rest.to_string(),
        None if canonical == "NaN" => canonical,
        None => format!("-{canonical}"),
    };
    // `-0.00` canonicalises back to `0.00`.
    Ok(numeric_bson(&canonical_numeric_text(&out)?))
}

/// The whole number nearest to a finite numeric text, ties away from zero
/// (PostgreSQL's `numeric -> int` cast rounds; `2.5::int` is 3, `-2.5::int`
/// is -3). `None` for NaN / Infinity.
pub fn numeric_text_to_integer(text: &str) -> Option<BigInt> {
    let d = Dec::parse(text)?;
    let den = BigInt::from(10u32).pow(d.scale);
    let q = round_half_away(&d.unscaled.abs(), &den);
    Some(if d.unscaled.is_negative() { -q } else { q })
}

/// A numeric text as an `f64`, the way PostgreSQL's `numeric -> float8` cast
/// reads it (nearest double; a huge magnitude becomes infinite).
pub fn numeric_text_to_f64(text: &str) -> Option<f64> {
    let canonical = canonical_numeric_text(text).ok()?;
    match canonical.as_str() {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => canonical.parse::<f64>().ok(),
    }
}

/// Exact sum of numeric texts with PostgreSQL's `sum(numeric)` scale (the
/// largest input scale). `None` if any input is not numeric.
pub fn sum_numeric_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<Bson> {
    let mut acc: Option<Dec> = None;
    let mut non_finite: Option<String> = None;
    for t in texts {
        let canonical = canonical_numeric_text(t).ok()?;
        if non_finite_class(&canonical).is_some() {
            non_finite = Some(match (non_finite.as_deref(), canonical.as_str()) {
                (Some("NaN"), _) | (_, "NaN") => "NaN".to_string(),
                (Some(prev), cur) if prev != cur => "NaN".to_string(),
                (_, cur) => cur.to_string(),
            });
            continue;
        }
        let d = Dec::parse(&canonical)?;
        acc = Some(match acc {
            None => d,
            Some(a) => {
                let scale = a.scale.max(d.scale);
                Dec {
                    unscaled: a.lift(scale) + d.lift(scale),
                    scale,
                }
            }
        });
    }
    if let Some(nf) = non_finite {
        return Some(numeric_bson(&nf));
    }
    acc.map(|d| numeric_bson(&d.render()))
}

/// `i128` form of a rounded integer, for the fixed-width integer casts.
pub fn bigint_to_i128(n: &BigInt) -> Option<i128> {
    n.to_i128()
}

// ---------------------------------------------------------------------------
// `numeric ^ numeric`
//
// A port of PostgreSQL's `numeric_power` / `power_var` / `power_var_int`
// (numeric.c), including their result-scale selection, which is what makes
// `2 ^ 0.5` render as `1.4142135623730950` and `10 ^ 2.5` as
// `316.22776601683793`: the scale is chosen so the result carries about
// sixteen significant digits, never fewer places than either operand, and
// for an integer exponent the base is multiplied out by repeated squaring at
// a bounded number of significant digits rather than exactly (an exact
// `1.0000001 ^ 100000000` would have seven hundred million places).
//
// The transcendental half (`ln` / `exp`, needed for a fractional exponent)
// is fixed-point BigInt arithmetic with guard digits; PostgreSQL's own is
// the same shape, so the two agree to the displayed scale.
// ---------------------------------------------------------------------------

/// `NUMERIC_MIN_SIG_DIGITS`.
const POWER_MIN_SIG_DIGITS: i64 = 16;
/// `NUMERIC_MAX_DISPLAY_SCALE`.
const POWER_MAX_DISPLAY_SCALE: i64 = 1000;
/// `NUMERIC_MAX_RESULT_SCALE`: `exp_var`'s overflow guard is `3 *` this on
/// the natural-log argument, `power_var`'s is `3.01 *` it.
const POWER_MAX_RESULT_SCALE: f64 = 2000.0;

impl Dec {
    fn from_bigint(n: BigInt) -> Dec {
        Dec {
            unscaled: n,
            scale: 0,
        }
    }

    fn is_negative(&self) -> bool {
        self.unscaled.is_negative()
    }

    fn abs(&self) -> Dec {
        Dec {
            unscaled: self.unscaled.abs(),
            scale: self.scale,
        }
    }

    /// Value rounded half away from zero to `scale` places (`round_var`).
    fn round_to(&self, scale: u32) -> Dec {
        if scale >= self.scale {
            return Dec {
                unscaled: self.lift(scale),
                scale,
            };
        }
        let den = BigInt::from(10u32).pow(self.scale - scale);
        let q = round_half_away(&self.unscaled.abs(), &den);
        Dec {
            unscaled: if self.is_negative() { -q } else { q },
            scale,
        }
    }

    /// Exact product rounded to `scale` places (`mul_var`).
    fn mul_round(&self, other: &Dec, scale: u32) -> Dec {
        Dec {
            unscaled: &self.unscaled * &other.unscaled,
            scale: self.scale + other.scale,
        }
        .round_to(scale)
    }

    /// `self / other` rounded to `scale` places (`div_var` with rounding);
    /// `other` must be non-zero.
    fn div_round(&self, other: &Dec, scale: u32) -> Dec {
        let num = self.unscaled.abs() * BigInt::from(10u32).pow(other.scale + scale);
        let den = other.unscaled.abs() * BigInt::from(10u32).pow(self.scale);
        let q = round_half_away(&num, &den);
        Dec {
            unscaled: if self.is_negative() != other.is_negative() {
                -q
            } else {
                q
            },
            scale,
        }
    }

    /// The integer digits of the absolute value (0 for a pure fraction).
    fn int_digits(&self) -> usize {
        let digits = self.unscaled.abs().to_string();
        digits.len().saturating_sub(self.scale as usize)
    }

    /// PostgreSQL's `weight`: the base-10000 position of the leading group.
    fn weight(&self) -> i64 {
        self.weight_and_first_group().0
    }

    fn to_f64(&self) -> f64 {
        self.render().parse::<f64>().unwrap_or(0.0)
    }

    /// `numeric_is_integral`: no non-zero digit after the point.
    fn is_integral(&self) -> bool {
        self.scale == 0 || (&self.unscaled % BigInt::from(10u32).pow(self.scale)).is_zero()
    }

    fn is_one(&self) -> bool {
        self.abs_cmp_one() == Ordering::Equal && !self.is_negative()
    }

    fn abs_cmp_one(&self) -> Ordering {
        self.unscaled
            .abs()
            .cmp(&BigInt::from(10u32).pow(self.scale))
    }
}

fn power_overflow() -> Error {
    Error::NumericOutOfRange("value overflows numeric format".to_string())
}

fn zero_with_scale(scale: i64) -> Dec {
    Dec {
        unscaled: BigInt::zero(),
        scale: scale.clamp(0, POWER_MAX_DISPLAY_SCALE) as u32,
    }
}

/// `estimate_ln_dweight`: the decimal weight of `ln(x)`, from the leading
/// digits alone -- unless `x` is within a tenth of one, where `ln(1 + d) ~= d`
/// gives a usable (possibly very negative) estimate instead.
fn estimate_ln_dweight(x: &Dec) -> i64 {
    let nine_tenths = Dec::parse("0.9").expect("literal");
    let eleven_tenths = Dec::parse("1.1").expect("literal");
    if compare_dec(x, &nine_tenths) != Ordering::Less
        && compare_dec(x, &eleven_tenths) != Ordering::Greater
    {
        let scale = x.scale;
        let diff = Dec {
            unscaled: &x.unscaled - BigInt::from(10u32).pow(scale),
            scale,
        };
        if diff.is_zero() {
            return 0;
        }
        let (weight, first) = diff.weight_and_first_group();
        return weight * 4 + (first as f64).log10() as i64;
    }
    let (weight, groups) = digit_groups(x);
    let mut approx = groups.first().copied().unwrap_or(0) as f64;
    let mut dweight = weight * 4;
    if let Some(second) = groups.get(1) {
        approx = approx * 10000.0 + *second as f64;
        dweight -= 4;
    }
    let ln = approx.ln() + dweight as f64 * std::f64::consts::LN_10;
    ln.abs().log10() as i64
}

/// `x` as PostgreSQL stores it: the base-10000 digit groups from the first
/// non-zero one, and the weight (base-10000 position) of that group.
fn digit_groups(x: &Dec) -> (i64, Vec<i64>) {
    let text = x.abs().render();
    let (int, frac) = text.split_once('.').unwrap_or((text.as_str(), ""));
    let int = int.trim_start_matches('0');
    // Groups are aligned to the decimal point: the integer part is grouped
    // from the right, the fraction from the left.
    let mut groups: Vec<i64> = Vec::new();
    if !int.is_empty() {
        let pad = (4 - int.len() % 4) % 4;
        let padded = format!("{}{}", "0".repeat(pad), int);
        for k in (0..padded.len()).step_by(4) {
            groups.push(padded[k..k + 4].parse().unwrap_or(0));
        }
    }
    let int_groups = groups.len() as i64;
    let mut k = 0;
    while k < frac.len() {
        let g = &frac[k..(k + 4).min(frac.len())];
        groups.push(format!("{g:0<4}").parse().unwrap_or(0));
        k += 4;
    }
    let Some(first) = groups.iter().position(|g| *g != 0) else {
        return (0, Vec::new());
    };
    (int_groups - 1 - first as i64, groups.split_off(first))
}

fn compare_dec(a: &Dec, b: &Dec) -> Ordering {
    let scale = a.scale.max(b.scale);
    a.lift(scale).cmp(&b.lift(scale))
}

/// Fixed-point value `n / 10^scale` helpers for the transcendental half.
struct Fixed {
    n: BigInt,
    scale: u32,
}

impl Fixed {
    fn one(scale: u32) -> Fixed {
        Fixed {
            n: BigInt::from(10u32).pow(scale),
            scale,
        }
    }

    fn from_dec(d: &Dec, scale: u32) -> Fixed {
        let n = if scale >= d.scale {
            d.lift(scale)
        } else {
            &d.unscaled / BigInt::from(10u32).pow(d.scale - scale)
        };
        Fixed { n, scale }
    }

    fn mul(&self, other: &Fixed) -> Fixed {
        Fixed {
            n: (&self.n * &other.n) / BigInt::from(10u32).pow(other.scale),
            scale: self.scale,
        }
    }

    fn div(&self, other: &Fixed) -> Fixed {
        Fixed {
            n: (&self.n * BigInt::from(10u32).pow(other.scale)) / &other.n,
            scale: self.scale,
        }
    }

    fn to_dec(&self) -> Dec {
        Dec {
            unscaled: self.n.clone(),
            scale: self.scale,
        }
    }
}

/// `ln(x)` for `x > 0` at `scale` places, computed with ten guard digits:
/// `x = m * 10^e`, `m` halved into `[1, 2)`, then the `atanh` series.
fn ln_fixed(x: &Dec, scale: u32) -> Dec {
    let work = scale + 12;
    let ten = BigInt::from(10u32);
    let one = ten.pow(work);
    // Split off the power of ten so that m is in [1, 10): shift the DECIMAL
    // first, so a tiny x is not truncated to zero on the way in.
    let e = if x.int_digits() > 0 {
        x.int_digits() as i64 - 1
    } else {
        let frac = x.abs().render();
        let frac = frac.split_once('.').map(|(_, f)| f).unwrap_or("");
        -(frac.chars().take_while(|c| *c == '0').count() as i64 + 1)
    };
    let mantissa = if e >= 0 {
        Dec {
            unscaled: x.unscaled.clone(),
            scale: x.scale + e as u32,
        }
    } else {
        Dec {
            unscaled: x.unscaled.clone(),
            scale: x.scale - e.unsigned_abs() as u32,
        }
    };
    let mut m = Fixed::from_dec(&mantissa, work);
    // m in [1, 10): halve into [1, 2).
    let two = BigInt::from(2u32);
    let mut halvings = 0i64;
    while m.n >= &one * &two {
        m.n /= &two;
        halvings += 1;
    }
    let ln2 = ln_series(
        &Fixed {
            n: &one * 2u32,
            scale: work,
        },
        work,
    );
    let ln10 = {
        // ln(10) = 3 ln(2) + ln(1.25)
        let five_quarters = Fixed {
            n: &one * 5u32 / 4u32,
            scale: work,
        };
        &ln2 * 3u32 + ln_series(&five_quarters, work)
    };
    let mut total = ln_series(&m, work);
    total += &ln2 * halvings;
    total += &ln10 * e;
    Dec {
        unscaled: total,
        scale: work,
    }
    .round_to(scale)
}

/// `ln(m)` for `m` in `[1, 2]` as a fixed-point integer at `scale`:
/// `2 * atanh(y)`, `y = (m - 1) / (m + 1)`.
fn ln_series(m: &Fixed, scale: u32) -> BigInt {
    let one = Fixed::one(scale);
    let y = Fixed {
        n: (&m.n - &one.n) * BigInt::from(10u32).pow(scale) / (&m.n + &one.n),
        scale,
    };
    let y2 = y.mul(&y);
    let mut term = y.n.clone();
    let mut sum = BigInt::zero();
    let mut k = 1u32;
    while !term.is_zero() {
        sum += &term / BigInt::from(k);
        term = &term * &y2.n / BigInt::from(10u32).pow(scale);
        k += 2;
    }
    sum * 2u32
}

/// `exp(z)` rounded to `scale` places (`exp_var`). `None` is an overflow.
fn exp_fixed(z: &Dec, scale: u32) -> Option<Dec> {
    let val = z.to_f64();
    if val.abs() >= POWER_MAX_RESULT_SCALE * 3.0 {
        return if val > 0.0 {
            None
        } else {
            Some(zero_with_scale(i64::from(scale)))
        };
    }
    let magnitude = (val * std::f64::consts::LOG10_E).abs().ceil() as u32 + 1;
    let work = scale + 16 + 2 * magnitude;
    let ten = BigInt::from(10u32);
    let one = ten.pow(work);
    // z = i + f with 0 <= f < 1.
    let zf = Fixed::from_dec(z, work);
    let i: BigInt = if zf.n.is_negative() {
        -((-&zf.n + &one - 1u32) / &one)
    } else {
        &zf.n / &one
    };
    let f = Fixed {
        n: &zf.n - &i * &one,
        scale: work,
    };
    // e^f by Taylor series.
    let mut term = one.clone();
    let mut sum = one.clone();
    let mut k = 1u32;
    while !term.is_zero() {
        term = &term * &f.n / &one / BigInt::from(k);
        sum += &term;
        k += 1;
    }
    let exp_f = Fixed {
        n: sum,
        scale: work,
    };
    // e^|i| by squaring, then invert for a negative i.
    let e = {
        let mut term = one.clone();
        let mut sum = one.clone();
        let mut k = 1u32;
        while !term.is_zero() {
            term /= BigInt::from(k);
            sum += &term;
            k += 1;
        }
        Fixed {
            n: sum,
            scale: work,
        }
    };
    let mut exponent = i.abs().to_u64().unwrap_or(u64::MAX);
    let mut acc = Fixed::one(work);
    let mut base = e;
    while exponent > 0 {
        if exponent & 1 == 1 {
            acc = acc.mul(&base);
        }
        exponent >>= 1;
        if exponent > 0 {
            base = base.mul(&base);
        }
    }
    let exp_i = if i.is_negative() {
        Fixed::one(work).div(&acc)
    } else {
        acc
    };
    let result = exp_i.mul(&exp_f).to_dec().round_to(scale);
    if result.int_digits() > MAX_INT_DIGITS {
        return None;
    }
    Some(result)
}

/// `power_var_int`: an integer exponent that fits `i32`.
fn power_int(base: &Dec, exp: i32, exp_dscale: u32) -> Result<Dec> {
    // Approximate the decimal weight of the result from the leading digits.
    let f = if base.is_zero() {
        0.0
    } else {
        // base ~= approx * 10^p, from its first sixteen digits.
        let (weight, groups) = digit_groups(base);
        let mut approx = groups.first().copied().unwrap_or(0) as f64;
        let mut p = weight * 4;
        for group in groups.iter().skip(1).take(3) {
            approx = approx * 10000.0 + *group as f64;
            p -= 4;
        }
        f64::from(exp) * (approx.log10() + p as f64)
    };
    if f > 3.0 * 32767.0 * 4.0 {
        return Err(power_overflow());
    }
    let rscale = (POWER_MIN_SIG_DIGITS - f as i64)
        .max(i64::from(base.scale))
        .max(i64::from(exp_dscale))
        .clamp(0, POWER_MAX_DISPLAY_SCALE);
    if f + 1.0 < -(rscale as f64) {
        return Ok(zero_with_scale(rscale));
    }
    let rscale = rscale as u32;
    match exp {
        0 => return Ok(Dec::from_bigint(BigInt::one()).round_to(rscale)),
        1 => return Ok(base.round_to(rscale)),
        -1 => return Ok(Dec::from_bigint(BigInt::one()).div_round(base, rscale)),
        2 => return Ok(base.mul_round(base, rscale)),
        _ => {}
    }
    if base.is_zero() {
        return Ok(zero_with_scale(i64::from(rscale)));
    }
    // Repeated squaring at a fixed number of significant digits: enough for
    // the result scale plus a margin for the error the multiplications
    // introduce (PostgreSQL sizes that margin with a NATURAL log).
    let sig_digits = 1 + i64::from(rscale) + f as i64 + f64::from(exp).abs().ln() as i64 + 8;
    let neg = exp < 0;
    let mut mask = exp.unsigned_abs();
    let mut base_prod = base.clone();
    let mut result = if mask & 1 == 1 {
        base.clone()
    } else {
        Dec::from_bigint(BigInt::one())
    };
    loop {
        mask >>= 1;
        if mask == 0 {
            break;
        }
        let local = (sig_digits - 2 * base_prod.weight() * 4)
            .min(2 * i64::from(base_prod.scale))
            .max(0) as u32;
        base_prod = base_prod.mul_round(&base_prod, local);
        if mask & 1 == 1 {
            let local = (sig_digits - (base_prod.weight() + result.weight()) * 4)
                .min(i64::from(base_prod.scale) + i64::from(result.scale))
                .max(0) as u32;
            result = base_prod.mul_round(&result, local);
        }
        if base_prod.weight() > 32767 || result.weight() > 32767 {
            // Overflow -- unless the exponent is negative, where the
            // reciprocal underflows to zero instead.
            if !neg {
                return Err(power_overflow());
            }
            return Ok(zero_with_scale(i64::from(rscale)));
        }
    }
    Ok(if neg {
        Dec::from_bigint(BigInt::one()).div_round(&result, rscale)
    } else {
        result.round_to(rscale)
    })
}

/// `power_var`: a base with a fractional exponent, via `exp(ln(base) * exp)`.
fn power_general(base: &Dec, exp: &Dec) -> Result<Dec> {
    if base.is_zero() {
        return Ok(zero_with_scale(POWER_MIN_SIG_DIGITS));
    }
    let abs_base = base.abs();
    // A negative base needs an integral exponent (too large for `i32`, or
    // the integer path would have taken it); the sign follows its parity.
    let negative_result = base.is_negative()
        && (&exp.round_to(0).unscaled % BigInt::from(2u32)).abs() == BigInt::one();
    let ln_dweight = estimate_ln_dweight(&abs_base);
    // This scale may exceed the display maximum when the base is very close
    // to one; PostgreSQL only floors it.
    let local = (8 - ln_dweight).max(0) as u32;
    let ln_base = ln_fixed(&abs_base, local);
    let ln_num = ln_base.mul_round(exp, local);
    let val = ln_num.to_f64();
    if val.abs() > POWER_MAX_RESULT_SCALE * 3.01 {
        return if val > 0.0 {
            Err(power_overflow())
        } else {
            Ok(zero_with_scale(POWER_MAX_DISPLAY_SCALE))
        };
    }
    // PostgreSQL's own truncated log10(e), kept verbatim so the integer
    // part of `val` -- which picks the result scale -- agrees with it.
    #[allow(clippy::approx_constant)]
    let val = val * 0.434294481903252;
    let rscale = (POWER_MIN_SIG_DIGITS - val as i64)
        .max(i64::from(base.scale))
        .max(i64::from(exp.scale))
        .clamp(0, POWER_MAX_DISPLAY_SCALE);
    let sig_digits = (rscale + val as i64).max(0);
    let local = (sig_digits - ln_dweight + 8).max(0) as u32;
    let ln_base = ln_fixed(&abs_base, local);
    let ln_num = ln_base.mul_round(exp, local);
    let result = exp_fixed(&ln_num, rscale as u32).ok_or_else(power_overflow)?;
    Ok(if negative_result && !result.is_zero() {
        Dec {
            unscaled: -result.unscaled,
            scale: result.scale,
        }
    } else {
        result
    })
}

/// `numeric ^ numeric` with PostgreSQL's result scale. `None` when either
/// operand is not a numeric text.
pub fn numeric_power(a: &str, b: &str) -> Option<Result<Bson>> {
    let (ca, cb) = (
        canonical_numeric_text(a).ok()?,
        canonical_numeric_text(b).ok()?,
    );
    Some(numeric_power_canonical(&ca, &cb).map(|t| numeric_bson(&t)))
}

pub(crate) fn numeric_power_canonical(a: &str, b: &str) -> Result<String> {
    let (ka, kb) = (non_finite_class(a), non_finite_class(b));
    let complex = || {
        Error::InvalidArgumentForPower(
            "a negative number raised to a non-integer power yields a complex result".into(),
        )
    };
    let zero_neg =
        || Error::InvalidArgumentForPower("zero raised to a negative power is undefined".into());
    if ka.is_some() || kb.is_some() {
        // POSIX pow(3): NaN ^ 0 = 1 and 1 ^ NaN = 1, every other NaN is NaN.
        if ka == Some(0) {
            let zero_exp = kb.is_none() && Dec::parse(b).is_some_and(|d| d.is_zero());
            return Ok(if zero_exp { "1" } else { "NaN" }.to_string());
        }
        if kb == Some(0) {
            let one_base = ka.is_none() && Dec::parse(a).is_some_and(|d| d.is_one());
            return Ok(if one_base { "1" } else { "NaN" }.to_string());
        }
        let sign = |k: Option<i8>, text: &str| -> i8 {
            match k {
                Some(k) => k,
                None => match Dec::parse(text) {
                    Some(d) if d.is_zero() => 0,
                    Some(d) if d.is_negative() => -1,
                    _ => 1,
                },
            }
        };
        let (s1, s2) = (sign(ka, a), sign(kb, b));
        let exp_integral = kb.is_some() || Dec::parse(b).is_some_and(|d| d.is_integral());
        if s1 == 0 && s2 < 0 {
            return Err(zero_neg());
        }
        if s1 < 0 && !exp_integral {
            return Err(complex());
        }
        if let Some(k2) = kb {
            // An infinite exponent: 1 and -1 give 1; |x| < 1 gives 0 for
            // +Inf and Infinity for -Inf, |x| > 1 the other way round.
            let base = Dec::parse(a);
            if base
                .as_ref()
                .is_some_and(|d| d.abs_cmp_one() == Ordering::Equal)
            {
                return Ok("1".to_string());
            }
            let small = base
                .as_ref()
                .is_some_and(|d| d.abs_cmp_one() == Ordering::Less);
            return Ok(if (k2 > 0) == small { "0" } else { "Infinity" }.to_string());
        }
        // An infinite base with a finite exponent.
        if s2 == 0 {
            return Ok("1".to_string());
        }
        if ka == Some(1) {
            return Ok(if s2 > 0 { "Infinity" } else { "0" }.to_string());
        }
        let odd = Dec::parse(b).is_some_and(|d| {
            d.is_integral() && (d.round_to(0).unscaled % BigInt::from(2u32)).abs() == BigInt::one()
        });
        return Ok(if s2 < 0 {
            "0"
        } else if odd {
            "-Infinity"
        } else {
            "Infinity"
        }
        .to_string());
    }
    let (base, exp) = (
        Dec::parse(a).ok_or_else(|| Error::InvalidText(a.to_string()))?,
        Dec::parse(b).ok_or_else(|| Error::InvalidText(b.to_string()))?,
    );
    if base.is_zero() && exp.is_negative() {
        return Err(zero_neg());
    }
    if base.is_negative() && !exp.is_integral() {
        return Err(complex());
    }
    let result = match exp
        .is_integral()
        .then(|| exp.round_to(0).unscaled.to_i32())
        .flatten()
    {
        Some(i) => power_int(&base, i, exp.scale)?,
        None => power_general(&base, &exp)?,
    };
    let text = result.render();
    if canonical_numeric_text(&text).is_err() {
        return Err(power_overflow());
    }
    Ok(text)
}

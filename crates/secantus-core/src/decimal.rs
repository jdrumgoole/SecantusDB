//! Exact decimal128 arithmetic — the numeric domain `$inc` / `$mul` / `$sum` /
//! `$avg` need when any operand is a `Decimal128`.
//!
//! Why this exists rather than a crate: `rust_decimal` tops out at 28
//! significant digits and decimal128 carries **34**, so it would reintroduce
//! exactly the truncation bug this module was written to fix. `NumVal` in
//! [`crate::numeric`] can't be reused either — it *normalizes* (strips trailing
//! zeros), and decimal128 is a non-normalized format whose **quantum** is
//! observable: mongod answers `2.50 + 0.10` with `2.60` and `2.50 * 2` with
//! `5.00`, not `2.6` / `5`. Arithmetic therefore has to preserve the exponent
//! the IEEE 754-2008 rules prescribe (`min(e1,e2)` for add, `e1+e2` for
//! multiply, `e1-e2` for an exact divide).
//!
//! Strategy: operands are aligned **exactly**, with no guard/round/sticky
//! bookkeeping — the working width is generous ([`WORK_DIGITS`]) and anything
//! wider defers to Python. That leaves exactly one place where rounding can
//! happen (a final round-half-even of an *exact* coefficient down to 34
//! digits), which is the property that makes this tractable to get right.
//!
//! Strings are the boundary in both directions: `bson::Decimal128` implements
//! `FromStr`/`Display`, and rendering through the decimal spec's
//! to-scientific-string form round-trips the coefficient and exponent — and so
//! the quantum — intact.

use bson::Bson;

/// decimal128's coefficient width.
const MAX_DIGITS: usize = 34;

/// How wide an exactly-aligned intermediate may get before we give up.
///
/// Sized to span decimal128's *entire* exponent range (`-6176 ..= 6111`) plus a
/// full 34-digit coefficient, so no pair of representable values can exceed it
/// and the exact-alignment strategy never has to fall back. That matters more
/// than the memory: a deferral is fatal on the standalone Rust server, which
/// has no Python to defer to. The generative parity fuzz caught the previous
/// 400-digit bound doing exactly that — a denormal double (`5e-324`, which
/// converts to roughly `E-357`) summed against an `E+25` decimal needs 401
/// digits to align, one past the old limit.
///
/// The wide buffers are only allocated for genuinely extreme spreads; ordinary
/// magnitudes touch a few dozen digits.
const WORK_DIGITS: usize = 12_400;

/// mongod converts a double to decimal128 at a fixed 15 significant digits.
const DOUBLE_SIG_DIGITS: usize = 15;

/// Significant digits sufficient to write any f64's exact decimal value.
const EXACT_EXPANSION_DIGITS: usize = 767;

/// A decimal128 value as sign / coefficient / exponent, **not** normalized:
/// trailing zeros in `coeff` are significant and are preserved by arithmetic.
/// Value is `sign * coeff * 10^exp`, `coeff` most-significant-digit first.
#[derive(Clone, Debug, PartialEq)]
pub enum Dec {
    Nan,
    /// `+1` / `-1`.
    Inf(i8),
    Fin {
        sign: i8,
        coeff: Vec<u8>,
        exp: i32,
    },
}

impl Dec {
    fn is_zero(&self) -> bool {
        matches!(self, Dec::Fin { coeff, .. } if coeff.iter().all(|d| *d == 0))
    }
}

/// Parse a decimal string, preserving trailing zeros and the exponent (and so
/// the quantum). Accepts the forms `Decimal128`'s `Display` emits plus plain
/// integers, `NaN`, and `Infinity`.
pub fn parse(s: &str) -> Option<Dec> {
    let t = s.trim();
    let low = t.to_ascii_lowercase();
    if low.contains("nan") {
        return Some(Dec::Nan);
    }
    if low.contains("inf") {
        return Some(Dec::Inf(if t.starts_with('-') { -1 } else { 1 }));
    }
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1i8, r),
        None => (1i8, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mantissa, exp_extra) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut coeff: Vec<u8> = Vec::with_capacity(int_part.len() + frac_part.len());
    for c in int_part.chars().chain(frac_part.chars()) {
        coeff.push(c.to_digit(10)? as u8);
    }
    // Leading zeros carry no information (unlike trailing ones); keep one digit
    // so zero still has a coefficient.
    while coeff.len() > 1 && coeff[0] == 0 {
        coeff.remove(0);
    }
    let exp = exp_extra.checked_sub(frac_part.len() as i32)?;
    Some(Dec::Fin { sign, coeff, exp })
}

/// Render in the decimal spec's to-scientific-string form — the same rendering
/// Python's `str(Decimal)` produces, and one `Decimal128::from_str` parses back
/// to an identical coefficient/exponent pair.
pub fn to_string(d: &Dec) -> String {
    match d {
        Dec::Nan => "NaN".to_string(),
        Dec::Inf(s) => {
            if *s < 0 {
                "-Infinity".into()
            } else {
                "Infinity".into()
            }
        }
        Dec::Fin { sign, coeff, exp } => {
            let digits: String = coeff.iter().map(|d| (b'0' + d) as char).collect();
            let adjusted = *exp as i64 + coeff.len() as i64 - 1;
            let neg = if *sign < 0 { "-" } else { "" };
            if *exp <= 0 && adjusted >= -6 {
                // Plain (non-exponential) notation.
                let body = if *exp == 0 {
                    digits
                } else if adjusted >= 0 {
                    let point = (adjusted + 1) as usize;
                    format!("{}.{}", &digits[..point], &digits[point..])
                } else {
                    format!("0.{}{}", "0".repeat((-adjusted - 1) as usize), digits)
                };
                format!("{neg}{body}")
            } else {
                let body = if digits.len() > 1 {
                    format!("{}.{}", &digits[..1], &digits[1..])
                } else {
                    digits
                };
                let esign = if adjusted < 0 { "-" } else { "+" };
                format!("{neg}{body}E{esign}{}", adjusted.abs())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// coefficient helpers (plain base-10 bignum over MSD-first digit vectors)
// ---------------------------------------------------------------------------

fn cmp_mag(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let (a, b) = (strip_leading(a), strip_leading(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn strip_leading(a: &[u8]) -> &[u8] {
    let mut i = 0;
    while i + 1 < a.len() && a[i] == 0 {
        i += 1;
    }
    &a[i..]
}

fn add_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u8;
    let (mut i, mut j) = (a.len(), b.len());
    while i > 0 || j > 0 || carry > 0 {
        let mut s = carry;
        if i > 0 {
            i -= 1;
            s += a[i];
        }
        if j > 0 {
            j -= 1;
            s += b[j];
        }
        out.push(s % 10);
        carry = s / 10;
    }
    out.reverse();
    out
}

/// `a - b`, requiring `a >= b` by magnitude.
fn sub_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i8;
    let (mut i, mut j) = (a.len(), b.len());
    while i > 0 {
        i -= 1;
        let mut d = a[i] as i8 - borrow;
        if j > 0 {
            j -= 1;
            d -= b[j] as i8;
        }
        if d < 0 {
            d += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u8);
    }
    out.reverse();
    let stripped = strip_leading(&out).to_vec();
    stripped
}

fn mul_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.iter().all(|d| *d == 0) || b.iter().all(|d| *d == 0) {
        return vec![0];
    }
    let mut acc = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate().rev() {
        for (j, &y) in b.iter().enumerate().rev() {
            acc[i + j + 1] += x as u32 * y as u32;
        }
    }
    for k in (1..acc.len()).rev() {
        let carry = acc[k] / 10;
        acc[k] %= 10;
        acc[k - 1] += carry;
    }
    let out: Vec<u8> = acc.into_iter().map(|d| d as u8).collect();
    strip_leading(&out).to_vec()
}

/// `coeff / n` and the remainder, for a divisor that fits in `u64`.
fn divmod_small(a: &[u8], n: u64) -> (Vec<u8>, u64) {
    let mut out = Vec::with_capacity(a.len());
    let mut rem: u128 = 0;
    for &d in a {
        rem = rem * 10 + d as u128;
        out.push((rem / n as u128) as u8);
        rem %= n as u128;
    }
    (strip_leading(&out).to_vec(), rem as u64)
}

/// Round an **exact** coefficient down to `keep` digits, half-even, returning
/// the new coefficient and the exponent increase it cost.
fn round_half_even(coeff: &[u8], keep: usize) -> (Vec<u8>, i32) {
    if coeff.len() <= keep {
        return (coeff.to_vec(), 0);
    }
    let dropped = coeff.len() - keep;
    let mut kept = coeff[..keep].to_vec();
    let first = coeff[keep];
    let rest_nonzero = coeff[keep + 1..].iter().any(|d| *d != 0);
    let last_odd = kept.last().is_some_and(|d| d % 2 == 1);
    let round_up = first > 5 || (first == 5 && (rest_nonzero || last_odd));
    let mut bump = dropped as i32;
    if round_up {
        kept = add_mag(&kept, &[1]);
        if kept.len() > keep {
            // 999… → 1000…: one more digit falls off the right.
            kept.truncate(keep);
            bump += 1;
        }
    }
    (kept, bump)
}

/// Round to decimal128's 34 digits and hand back a `Dec`.
fn finish(sign: i8, coeff: Vec<u8>, exp: i32) -> Option<Dec> {
    // Leading zeros aren't significant, and rounding counts digits from the
    // left — so an unstripped coefficient (aligning `0E+10` against `1E-28`
    // yields 38 leading zeros) would "round" by keeping the zeros and throwing
    // the real digits away.
    let coeff = strip_leading(&coeff).to_vec();
    let (coeff, bump) = round_half_even(&coeff, MAX_DIGITS);
    let exp = exp.checked_add(bump)?;
    Some(Dec::Fin { sign, coeff, exp })
}

/// Widen `coeff` so it reads at exponent `target` (`target <= exp`).
fn scale_to(coeff: &[u8], exp: i32, target: i32) -> Vec<u8> {
    let pad = (exp - target) as usize;
    let mut out = Vec::with_capacity(coeff.len() + pad);
    out.extend_from_slice(coeff);
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

// ---------------------------------------------------------------------------
// arithmetic
// ---------------------------------------------------------------------------

/// `a + b`. `None` when the exact form is too wide to align (defer to Python).
pub fn add(a: &Dec, b: &Dec) -> Option<Dec> {
    use Dec::*;
    match (a, b) {
        (Nan, _) | (_, Nan) => Some(Nan),
        // Opposite infinities are NaN; like ones absorb.
        (Inf(x), Inf(y)) => Some(if x == y { Inf(*x) } else { Nan }),
        (Inf(x), _) => Some(Inf(*x)),
        (_, Inf(y)) => Some(Inf(*y)),
        (
            Fin {
                sign: s1,
                coeff: c1,
                exp: e1,
            },
            Fin {
                sign: s2,
                coeff: c2,
                exp: e2,
            },
        ) => {
            // IEEE: the preferred exponent of a sum is min(e1, e2).
            let target = (*e1).min(*e2);
            let width = c1.len().max(c2.len()) + (e1 - target).max(e2 - target) as usize;
            if width > WORK_DIGITS {
                return None;
            }
            let x = scale_to(c1, *e1, target);
            let y = scale_to(c2, *e2, target);
            let (sign, mag) = if s1 == s2 {
                (*s1, add_mag(&x, &y))
            } else {
                match cmp_mag(&x, &y) {
                    std::cmp::Ordering::Equal => {
                        // Exact cancellation still keeps the preferred exponent
                        // (mongod: 2.50 - 2.50 is 0.00, not 0).
                        return finish(1, vec![0], target);
                    }
                    std::cmp::Ordering::Greater => (*s1, sub_mag(&x, &y)),
                    std::cmp::Ordering::Less => (*s2, sub_mag(&y, &x)),
                }
            };
            finish(sign, mag, target)
        }
    }
}

/// `a * b`.
pub fn mul(a: &Dec, b: &Dec) -> Option<Dec> {
    use Dec::*;
    match (a, b) {
        (Nan, _) | (_, Nan) => Some(Nan),
        // 0 * Infinity is NaN; otherwise the sign carries.
        (Inf(x), other) | (other, Inf(x)) => {
            if other.is_zero() {
                return Some(Nan);
            }
            let s = match other {
                Fin { sign, .. } => *sign,
                Inf(y) => *y,
                Nan => return Some(Nan),
            };
            Some(Inf(x * s))
        }
        (
            Fin {
                sign: s1,
                coeff: c1,
                exp: e1,
            },
            Fin {
                sign: s2,
                coeff: c2,
                exp: e2,
            },
        ) => {
            // IEEE: the preferred exponent of a product is e1 + e2.
            let exp = e1.checked_add(*e2)?;
            finish(s1 * s2, mul_mag(c1, c2), exp)
        }
    }
}

/// `a / n` for a positive integer count — the shape `$avg` needs. Mirrors
/// CPython's `Decimal.__truediv__`: divide to one digit past the working
/// precision, nudge a truncated quotient so the final half-even round lands the
/// way the exact value would, and on an exact quotient walk the exponent back
/// toward the ideal `e1 - 0`.
pub fn div_int(a: &Dec, n: i64) -> Option<Dec> {
    use Dec::*;
    if n == 0 {
        return None;
    }
    let (nsign, nmag) = if n < 0 {
        (-1i8, n.unsigned_abs())
    } else {
        (1i8, n as u64)
    };
    match a {
        Nan => Some(Nan),
        Inf(x) => Some(Inf(x * nsign)),
        Fin {
            sign: s1,
            coeff: c1,
            exp: e1,
        } => {
            if c1.iter().all(|d| *d == 0) {
                return Some(Fin {
                    sign: s1 * nsign,
                    coeff: vec![0],
                    exp: *e1,
                });
            }
            let ndigits = nmag.to_string().len();
            let shift = ndigits as i64 - c1.len() as i64 + MAX_DIGITS as i64 + 1;
            // `shift` is positive for every decimal128 coefficient (c1 has at
            // most 34 digits), so only the widening branch can be taken.
            if shift <= 0 || shift > WORK_DIGITS as i64 {
                return None;
            }
            let mut wide = c1.clone();
            wide.extend(std::iter::repeat_n(0u8, shift as usize));
            let (mut q, rem) = divmod_small(&wide, nmag);
            let mut exp = (*e1 as i64).checked_sub(shift)?;
            if rem != 0 {
                // Inexact: make the truncated quotient round like the exact one
                // (a quotient ending in 0 or 5 is the only case where the
                // dropped remainder can change a half-even decision).
                if q.last().is_some_and(|d| d % 5 == 0) {
                    q = add_mag(&q, &[1]);
                }
            } else {
                let ideal = *e1 as i64;
                while exp < ideal && q.len() > 1 && q.last() == Some(&0) {
                    q.pop();
                    exp += 1;
                }
            }
            let exp: i32 = exp.try_into().ok()?;
            finish(s1 * nsign, q, exp)
        }
    }
}

// ---------------------------------------------------------------------------
// BSON boundary
// ---------------------------------------------------------------------------

/// A BSON number as an exact decimal. Doubles go through their shortest
/// round-trip form (`0.1` → `0.1`, not `0.1000000000000000055…`), matching
/// `secantus.numerics._as_decimal`'s `Decimal(str(float))`.
pub fn from_bson(b: &Bson) -> Option<Dec> {
    match b {
        Bson::Int32(i) => parse(&i.to_string()),
        Bson::Int64(i) => parse(&i.to_string()),
        Bson::Double(d) => {
            if d.is_nan() {
                Some(Dec::Nan)
            } else if d.is_infinite() {
                Some(Dec::Inf(if *d > 0.0 { 1 } else { -1 }))
            } else if *d == 0.0 {
                // mongod renders a zero double as plain `0` / `-0`, unpadded.
                Some(Dec::Fin {
                    sign: if d.is_sign_negative() { -1 } else { 1 },
                    coeff: vec![0],
                    exp: 0,
                })
            } else {
                // mongod converts a double at a fixed 15 significant digits,
                // rounding the **exact** binary value (not the shortest repr —
                // they part company at the denormal edge, where `5e-324`
                // converts to 4.94065645841247E-324). `{:.14e}` is exactly
                // that: 1 + 14 digits, correctly rounded from the true value.
                parse(&format!("{:.*e}", DOUBLE_SIG_DIGITS - 1, d))
            }
        }
        Bson::Decimal128(d) => parse(&d.to_string()),
        _ => None,
    }
}

/// A BSON number as an exact decimal for the **accumulator** rule.
///
/// `$sum` / `$avg` do not use [`from_bson`]'s 15-digit conversion: they take a
/// double's exact binary value, capped at decimal128's 34 digits (probed
/// 6.0.16). `$inc` by `0.1` moves a decimal by `0.100000000000000`; `$sum` of
/// `0.1` contributes `0.1000000000000000055511151231257827`. A double that is
/// already exact keeps its short form, so `$sum` of `3.0` is `3`.
pub fn from_bson_accumulator(b: &Bson) -> Option<Dec> {
    let Bson::Double(d) = b else {
        return from_bson(b);
    };
    if d.is_nan() {
        return Some(Dec::Nan);
    }
    if d.is_infinite() {
        return Some(Dec::Inf(if *d > 0.0 { 1 } else { -1 }));
    }
    if *d == 0.0 {
        return Some(Dec::Fin {
            sign: if d.is_sign_negative() { -1 } else { 1 },
            coeff: vec![0],
            exp: 0,
        });
    }
    // 767 significant digits is enough to write any f64 exactly; the trailing
    // zeros are then dropped so an exactly-representable double (3.0) keeps its
    // short form, and `finish` applies decimal128's 34-digit cap the way mongod
    // does on the way in.
    let s = format!("{d:.*e}", EXACT_EXPANSION_DIGITS - 1);
    match parse(&s)? {
        Dec::Fin {
            sign,
            mut coeff,
            mut exp,
        } => {
            // Strip only down to exponent 0, never past it: an integral double
            // converts with the zeros *in the coefficient* (mongod and
            // CPython's `Decimal(float)` both answer 1e10 with `10000000000`,
            // not `1E+10`), and the two spellings differ in quantum.
            while coeff.len() > 1 && coeff.last() == Some(&0) && exp < 0 {
                coeff.pop();
                exp += 1;
            }
            finish(sign, coeff, exp)
        }
        other => Some(other),
    }
}

/// Back to BSON. `None` when the value falls outside what decimal128 can hold
/// (extreme exponents) — the caller defers rather than inventing a result.
pub fn to_bson(d: &Dec) -> Option<Bson> {
    to_string(d)
        .parse::<bson::Decimal128>()
        .ok()
        .map(Bson::Decimal128)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Dec {
        parse(s).unwrap()
    }
    fn add_s(a: &str, b: &str) -> String {
        to_string(&add(&d(a), &d(b)).unwrap())
    }
    fn mul_s(a: &str, b: &str) -> String {
        to_string(&mul(&d(a), &d(b)).unwrap())
    }
    fn div_s(a: &str, n: i64) -> String {
        to_string(&div_int(&d(a), n).unwrap())
    }

    #[test]
    fn parse_render_roundtrip_preserves_quantum() {
        for s in [
            "2.50", "0", "0.00", "-3", "1E+10", "1.5E-8", "100", "0.001", "-0.0",
        ] {
            assert_eq!(to_string(&d(s)), s, "roundtrip {s}");
        }
    }

    #[test]
    fn add_keeps_the_full_34_digits() {
        // The bug this module exists for: a 28-digit context truncated this.
        assert_eq!(
            add_s("1.000000000000000000000000000000001", "1"),
            "2.000000000000000000000000000000001"
        );
    }

    #[test]
    fn add_uses_the_min_exponent_as_mongod_does() {
        assert_eq!(add_s("2.50", "0.10"), "2.60");
        assert_eq!(add_s("2.50", "0"), "2.50");
        assert_eq!(add_s("1", "2"), "3");
        assert_eq!(add_s("0.1", "0.2"), "0.3");
    }

    #[test]
    fn add_with_opposite_signs() {
        assert_eq!(add_s("5", "-3"), "2");
        assert_eq!(add_s("-5", "3"), "-2");
        assert_eq!(add_s("2.50", "-2.50"), "0.00");
        assert_eq!(add_s("1.5", "-3.5"), "-2.0");
    }

    #[test]
    fn mul_sums_the_exponents() {
        assert_eq!(mul_s("2.50", "2"), "5.00");
        assert_eq!(mul_s("0.1", "0.1"), "0.01");
        assert_eq!(mul_s("-3", "4"), "-12");
        assert_eq!(mul_s("2.5", "0"), "0.0");
    }

    #[test]
    fn mul_rounds_half_even_past_34_digits() {
        // mongod-probed (6.0.16).
        assert_eq!(
            mul_s("1.234567890123456789012345678901234", "9.999"),
            "12.34444433334444443333444444333344"
        );
    }

    #[test]
    fn div_matches_cpython_decimal() {
        assert_eq!(div_s("10", 2), "5");
        assert_eq!(div_s("2.50", 2), "1.25");
        assert_eq!(div_s("1", 3), "0.3333333333333333333333333333333333");
        // mongod-probed: $avg of [1.000…001, 1] keeps 34 digits.
        assert_eq!(
            div_s("2.000000000000000000000000000000001", 2),
            "1.000000000000000000000000000000000"
        );
        assert_eq!(div_s("-9", 3), "-3");
    }

    #[test]
    fn specials_propagate() {
        assert_eq!(to_string(&add(&Dec::Nan, &d("1")).unwrap()), "NaN");
        assert_eq!(to_string(&add(&Dec::Inf(1), &d("1")).unwrap()), "Infinity");
        assert_eq!(to_string(&add(&Dec::Inf(1), &Dec::Inf(-1)).unwrap()), "NaN");
        assert_eq!(
            to_string(&mul(&Dec::Inf(1), &d("-2")).unwrap()),
            "-Infinity"
        );
        assert_eq!(to_string(&mul(&Dec::Inf(1), &d("0")).unwrap()), "NaN");
    }

    #[test]
    fn double_converts_at_15_significant_digits() {
        // mongod-probed (6.0.16): the conversion is fixed-width, and rounds the
        // exact binary value — hence 5e-324, where the shortest repr disagrees.
        for (f, want) in [
            (3.0, "3.00000000000000"),
            (0.1, "0.100000000000000"),
            (-2.5, "-2.50000000000000"),
            (123.456, "123.456000000000"),
            (1e10, "10000000000.0000"),
            (1e16, "1.00000000000000E+16"),
            (1e-5, "0.0000100000000000000"),
            (1.0 / 3.0, "0.333333333333333"),
            (5e-324, "4.94065645841247E-324"),
            (0.0, "0"),
            (-0.0, "-0"),
        ] {
            let got = to_string(&from_bson(&Bson::Double(f)).unwrap());
            assert_eq!(got, want, "double {f:e}");
        }
    }

    #[test]
    fn accumulators_convert_doubles_exactly_not_at_15_digits() {
        // mongod-probed (6.0.16): $sum/$avg and $inc/$mul genuinely disagree on
        // how a double enters the decimal domain.
        let acc = |f: f64| to_string(&from_bson_accumulator(&Bson::Double(f)).unwrap());
        let upd = |f: f64| to_string(&from_bson(&Bson::Double(f)).unwrap());

        assert_eq!(acc(0.1), "0.1000000000000000055511151231257827");
        assert_eq!(upd(0.1), "0.100000000000000");
        // An exactly-representable double keeps its short form either way.
        assert_eq!(acc(3.0), "3");
        assert_eq!(upd(3.0), "3.00000000000000");
        assert_eq!(acc(1.5), "1.5");
        assert_eq!(acc(0.0), "0");
        assert_eq!(acc(-0.0), "-0");
        // An integral double keeps its zeros in the coefficient (exponent 0),
        // rather than collapsing to a positive exponent — mongod-probed.
        assert_eq!(acc(1e10), "10000000000");
        assert_eq!(acc(1e16), "10000000000000000");
        assert_eq!(acc(2500.0), "2500");
    }

    #[test]
    fn bson_roundtrip() {
        let v = from_bson(&Bson::Double(0.1)).unwrap();
        assert_eq!(to_string(&v), "0.100000000000000");
        assert_eq!(to_string(&from_bson(&Bson::Int32(-7)).unwrap()), "-7");
        assert!(matches!(to_bson(&d("2.50")), Some(Bson::Decimal128(_))));
    }

    #[test]
    fn zero_with_a_wide_exponent_keeps_the_other_operand() {
        // Regression: aligning a zero coefficient against a far-away exponent
        // produced leading zeros that rounding then mistook for significant
        // digits, truncating the real answer.
        assert_eq!(add_s("-0E+10", "-7.56E-26"), "-7.56E-26");
        assert_eq!(add_s("0E+10", "0E-28"), "0E-28");
        assert_eq!(
            add_s("1.128797342904130E-13", "0E+10"),
            "1.128797342904130E-13"
        );
    }

    #[test]
    fn enormous_exponent_spread_still_computes() {
        // Nothing representable as decimal128 may defer — the Rust server has
        // no Python to fall back to. The tiny addend survives only as a
        // rounding influence, which is exactly CPython's answer at prec 34.
        assert_eq!(
            add_s("1E+500", "1E-500"),
            "1.000000000000000000000000000000000E+500"
        );
        // The denormal-vs-large pairing the parity fuzz found.
        let tiny = from_bson_accumulator(&Bson::Double(5e-324)).unwrap();
        assert!(add(&tiny, &d("9.949442263900951E+25")).is_some());
        // Both ends of decimal128's exponent range at once.
        assert!(add(&d("1E+6111"), &d("1E-6176")).is_some());
    }
}

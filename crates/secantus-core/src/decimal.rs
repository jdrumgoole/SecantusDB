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
    /// A FINITE zero of either sign. `Dec::Fin` in the pattern is the finite
    /// check -- a NaN or an infinity never matches. Public because `$mul`'s
    /// stored-zero rule needs it (see `update::is_zero_number`).
    pub fn is_zero(&self) -> bool {
        matches!(self, Dec::Fin { coeff, .. } if coeff.iter().all(|d| *d == 0))
    }
}

/// `-a`, preserving the coefficient and exponent (and so the quantum).
///
/// `$subtract` is `add` with the right operand negated -- mongod's own
/// identity, and the reason this needs no separate subtraction routine.
pub fn neg(a: &Dec) -> Dec {
    match a {
        Dec::Fin { sign, coeff, exp } => Dec::Fin {
            sign: -sign,
            coeff: coeff.clone(),
            exp: *exp,
        },
        other => other.clone(),
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

/// Which way a value that falls between two representable ones is moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundMode {
    /// Toward +infinity (`$ceil`).
    Ceil,
    /// Toward -infinity (`$floor`).
    Floor,
    /// Toward zero (`$trunc`).
    Trunc,
    /// Nearest, ties to even (`$round`). 2.5 -> 2 and 3.5 -> 4.
    HalfEven,
}

/// Round `d` so its exponent is at least `target_exp`, in `mode`.
///
/// The RESULT keeps `target_exp` as its quantum, which is what makes
/// `{$round: [Decimal128("2.567"), 2]}` answer `2.57` rather than `2.5700…`
/// and `{$round: [Decimal128("25"), -1]}` answer `2E+1`. A value already
/// coarser than the target is returned unchanged -- `{$ceil:
/// Decimal128("2.00")}` is `2`, not `2.00`, because ceil targets exponent 0
/// and 2.00 rounds up into it. Probed 8.2.11 (2026-09-03).
pub fn round_to_exp(d: &Dec, target_exp: i32, mode: RoundMode) -> Option<Dec> {
    let Dec::Fin { sign, coeff, exp } = d else {
        // NaN and the infinities pass through every rounding operator.
        return Some(d.clone());
    };
    if *exp >= target_exp {
        // Already at or COARSER than the target: nothing to drop, but the
        // result still carries the requested quantum, so pad it out.
        // `{$round: [Decimal128("2.5"), 2]}` is `2.50`, not `2.5` -- the place
        // sets the quantum whether or not it changed the value (probed 8.2.11,
        // 2026-09-03; returning it unchanged was wrong on 40 of 210 shapes).
        return finish(*sign, scale_to(coeff, *exp, target_exp), target_exp);
    }
    let drop = (target_exp - *exp) as usize;
    let keep = coeff.len().saturating_sub(drop);
    let dropped_nonzero = coeff[keep.min(coeff.len())..].iter().any(|x| *x != 0);
    let mut kept: Vec<u8> = coeff[..keep].to_vec();
    if kept.is_empty() {
        kept.push(0);
    }
    // The digit that decides the rounding sits at index `len - drop`. When
    // `drop` EXCEEDS the coefficient's length that position is a leading
    // implicit zero, not `coeff[0]` -- the whole value lies below the target
    // place. Reading `coeff[0]` there rounded `9.995` to the nearest thousand
    // as `1E+3` where mongod answers `0E+3`.
    let deciding = if drop < coeff.len() {
        coeff[keep]
    } else if drop == coeff.len() {
        coeff[0]
    } else {
        0
    };
    let after_deciding_nonzero = if drop <= coeff.len() {
        coeff[(keep + 1).min(coeff.len())..].iter().any(|x| *x != 0)
    } else {
        // Everything the coefficient holds sits strictly below the deciding
        // position.
        coeff.iter().any(|x| *x != 0)
    };
    let round_away = match mode {
        RoundMode::Trunc => false,
        // Ceil moves away from zero only for a POSITIVE value, floor only for
        // a negative one -- they are directional, not magnitude-based.
        RoundMode::Ceil => dropped_nonzero && *sign > 0,
        RoundMode::Floor => dropped_nonzero && *sign < 0,
        RoundMode::HalfEven => {
            let last_odd = kept.last().is_some_and(|x| x % 2 == 1);
            deciding > 5 || (deciding == 5 && (after_deciding_nonzero || last_odd))
        }
    };
    if round_away {
        kept = add_mag(&kept, &[1]);
    }
    finish(*sign, kept, target_exp)
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
    to_string(&clamp(d))
        .parse::<bson::Decimal128>()
        .ok()
        .map(Bson::Decimal128)
}

/// `|a|` against `|b|`, exactly and at any width.
///
/// Neither operand is rounded, so this separates values that a 34-digit
/// subtraction would collapse -- which is what the `f64` normal-range boundary
/// needs. `None` when either side is NaN.
pub fn cmp_abs(a: &Dec, b: &Dec) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let rank = |d: &Dec| match d {
        Dec::Nan => 2,
        Dec::Inf(_) => 1,
        Dec::Fin { .. } => 0,
    };
    if rank(a) == 2 || rank(b) == 2 {
        return None;
    }
    match rank(a).cmp(&rank(b)) {
        Ordering::Equal => {}
        other => return Some(other),
    }
    let (
        Dec::Fin {
            coeff: ca, exp: ea, ..
        },
        Dec::Fin {
            coeff: cb, exp: eb, ..
        },
    ) = (a, b)
    else {
        return Some(Ordering::Equal); // both infinite
    };
    let (ma, mb) = (strip_leading(ca), strip_leading(cb));
    let (za, zb) = (ma.iter().all(|d| *d == 0), mb.iter().all(|d| *d == 0));
    if za || zb {
        return Some(match (za, zb) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            _ => Ordering::Greater,
        });
    }
    // Compare the exponent of the LEADING digit first; only equal magnitudes
    // need the digits aligned.
    let (adj_a, adj_b) = (ea + ma.len() as i32 - 1, eb + mb.len() as i32 - 1);
    match adj_a.cmp(&adj_b) {
        Ordering::Equal => {}
        other => return Some(other),
    }
    let width = ma.len().max(mb.len());
    let pad = |m: &[u8]| {
        let mut v = m.to_vec();
        v.resize(width, 0);
        v
    };
    Some(cmp_mag(&pad(ma), &pad(mb)))
}

/// decimal128's smallest exponent -- the quantum of a subnormal.
const MIN_EXP: i32 = -6176;
/// decimal128's largest exponent, for a single-digit coefficient.
const MAX_EXP: i32 = 6111;
/// The exponent of the LEADING digit above which the value overflows.
const MAX_ADJUSTED: i32 = 6144;

/// A `Dec` brought inside decimal128's exponent range.
///
/// The arbitrary-precision `Dec` can hold values the format cannot, and
/// `bson::Decimal128`'s parser REFUSES those rather than clamping -- so
/// `$radiansToDegrees(Decimal128("1E-6176"))`, whose exact product is
/// `5.729577951308232087679815481410517E-6175`, came back as `None` and the
/// Rust server answered a `BadValue` where mongod answers `5.7E-6175`. The two
/// rules are the format's own:
///
/// - past the top, the result is `+-Infinity` (mongod: `1E+6144` radians in
///   degrees is `Infinity`);
/// - past the bottom, the coefficient is ROUNDED to the minimum quantum, which
///   is where subnormals come from -- `5.7E-6175` keeps two digits of the 34,
///   and a value small enough rounds all the way to `0E-6176`.
///
/// Measured against 8.2.11, 2026-09-07.
fn clamp(d: &Dec) -> Dec {
    let Dec::Fin { sign, coeff, exp } = d else {
        return d.clone();
    };
    let mag = strip_leading(coeff);
    if mag.iter().all(|x| *x == 0) {
        // A zero carries no digits to trade, so only its quantum is clamped.
        return Dec::Fin {
            sign: *sign,
            coeff: vec![0],
            exp: (*exp).clamp(MIN_EXP, MAX_EXP),
        };
    }
    if *exp + mag.len() as i32 - 1 > MAX_ADJUSTED {
        return Dec::Inf(*sign);
    }
    if *exp > MAX_EXP {
        // Room to spare below: shift digits out of the exponent into the
        // coefficient. The overflow test above bounds the result at 34 digits.
        let mut c = mag.to_vec();
        c.extend(std::iter::repeat_n(0u8, (*exp - MAX_EXP) as usize));
        return Dec::Fin {
            sign: *sign,
            coeff: c,
            exp: MAX_EXP,
        };
    }
    if *exp < MIN_EXP {
        return round_to_exp(d, MIN_EXP, RoundMode::HalfEven).unwrap_or(Dec::Fin {
            sign: *sign,
            coeff: vec![0],
            exp: MIN_EXP,
        });
    }
    d.clone()
}

/// The integer part of a finite decimal, truncated TOWARD ZERO, when it fits in
/// an `i64`. `$mod` needs exactly this and nothing more: mongod truncates an
/// int / long / double / Decimal128 operand toward zero before taking the
/// remainder (probed 7.0.12).
///
/// Deliberately not routed through `f64`: a Decimal128 carries 34 significant
/// digits and a double 17, so `"12345678901234567890.5"` would round before the
/// modulo and answer a remainder mongod does not report. Working on the
/// coefficient digits keeps it exact.
pub fn trunc_to_i64(d: &Dec) -> Option<i64> {
    let Dec::Fin { sign, coeff, exp } = d else {
        return None; // NaN / Infinity contribute nothing to $mod
    };
    // 19 digits is i64's width; past that the accumulate below would overflow
    // anyway, and this keeps a huge positive exponent from allocating.
    if *exp > 19 {
        return None;
    }
    let mut digits: Vec<u8> = coeff.clone();
    if *exp < 0 {
        let drop = exp.unsigned_abs() as usize;
        if drop >= digits.len() {
            return Some(0); // everything was fractional
        }
        digits.truncate(digits.len() - drop);
    } else {
        digits.resize(digits.len() + *exp as usize, 0);
    }
    let mut acc: i64 = 0;
    for digit in digits {
        acc = acc.checked_mul(10)?.checked_add(digit as i64)?;
    }
    Some(acc * *sign as i64)
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

// --- square root ---------------------------------------------------------

/// `floor(sqrt(n))` and its remainder, both exact, by the digit-by-digit
/// ("long division") square-root algorithm.
///
/// At each step the next root digit `d` is the largest with
/// `(20*root + d) * d <= remainder`, which needs only comparison, subtraction
/// and multiplication by a single digit -- so it works directly on the
/// most-significant-first digit vectors this module uses, with no big-integer
/// division. `n` is padded to an even length so it can be consumed in pairs.
fn isqrt_mag(n: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut digits = Vec::with_capacity(n.len() + 1);
    if n.len() % 2 == 1 {
        digits.push(0);
    }
    digits.extend_from_slice(n);

    let mut root: Vec<u8> = Vec::with_capacity(digits.len() / 2);
    let mut rem: Vec<u8> = Vec::new();
    for pair in digits.chunks(2) {
        // rem = rem * 100 + pair
        rem.push(pair[0]);
        rem.push(pair[1]);
        let r = strip_leading(&rem).to_vec();
        rem = r;

        // twenty_root = root * 20
        let twenty_root = mul_mag(&root, &[2, 0]);
        let mut best = 0u8;
        let mut best_prod: Vec<u8> = vec![0];
        for d in 1..=9u8 {
            let cand = add_mag(&twenty_root, &[d]);
            let prod = mul_mag(&cand, &[d]);
            if cmp_mag(&prod, &rem) == std::cmp::Ordering::Greater {
                break;
            }
            best = d;
            best_prod = prod;
        }
        rem = strip_leading(&sub_mag(&rem, &best_prod)).to_vec();
        root.push(best);
    }
    (strip_leading(&root).to_vec(), strip_leading(&rem).to_vec())
}

/// The decimal square root, correctly rounded to 34 significant digits.
///
/// `None` for a NEGATIVE operand (including `-Infinity`), which mongod rejects
/// with `28714 $sqrt's argument must be greater than or equal to 0` -- the
/// caller raises it, so this stays free of error text.
///
/// Two rules, both measured against mongod 8.2.11 (2026-09-08):
///
/// * **Correct rounding.** IEEE 754 requires it for square root (unlike the
///   transcendentals, where mongod's own answer is 1-2 ULP off the true value --
///   see `tools/probes/decimal_transcendental_rule.py`). The digit-by-digit
///   root is EXACT, so a single guard digit plus the "is the remainder zero"
///   sticky bit decides the rounding with no error analysis.
/// * **The IDEAL EXPONENT.** `$sqrt` is not always 34 digits: an exact result is
///   expressed at exponent `floor(e/2)`, which is why `sqrt(4)` is `2`,
///   `sqrt(0.25)` is `0.5`, `sqrt(100)` is `10` and `sqrt(0.00)` is `0.0` rather
///   than any of them padded out. An inexact result keeps the full 34.
pub fn sqrt(a: &Dec) -> Option<Dec> {
    match a {
        Dec::Nan => Some(Dec::Nan),
        Dec::Inf(1) => Some(Dec::Inf(1)),
        Dec::Inf(_) => None, // -Infinity is out of domain
        Dec::Fin { sign, coeff, exp } => {
            let mag = strip_leading(coeff);
            let is_zero = mag.iter().all(|d| *d == 0);
            if *sign < 0 && !is_zero {
                return None;
            }
            let ideal = exp.div_euclid(2);
            if is_zero {
                // sqrt(±0) is ±0 at the ideal exponent, sign preserved.
                return Some(Dec::Fin {
                    sign: *sign,
                    coeff: vec![0],
                    exp: ideal,
                });
            }

            // Scale so the exact integer root lands on 35 digits: one guard
            // digit past decimal128's 34. `n = coeff * 10^k` must have 69 or 70
            // digits, and `e - k` must be EVEN so the result exponent
            // `f = (e - k) / 2` is an integer -- both `k` candidates are
            // available, so parity is always satisfiable.
            let nc = mag.len();
            let mut k = 69usize.saturating_sub(nc);
            if (exp - k as i32) % 2 != 0 {
                k += 1;
            }
            let mut n = mag.to_vec();
            n.extend(std::iter::repeat_n(0u8, k));
            let f = (exp - k as i32) / 2;

            let (root, rem) = isqrt_mag(&n);
            // "Exact" means representable: the integer root is exact AND the
            // guard digits about to be dropped are zeros. The root is 35 digits
            // by construction, so a 35-significant-digit root is inexact for
            // decimal128 even when the remainder is zero.
            let exact = (rem.is_empty() || rem.iter().all(|d| *d == 0))
                && root[MAX_DIGITS.min(root.len())..].iter().all(|d| *d == 0);

            // Round the 35-digit root to 34: the dropped digit is the guard and
            // a non-zero remainder is the sticky bit, so a tie only stays a tie
            // when the root is exact.
            let (coeff, exp2) = if root.len() > MAX_DIGITS {
                let keep = MAX_DIGITS;
                let dropped = root.len() - keep;
                let mut kept = root[..keep].to_vec();
                let guard = root[keep];
                let rem_nonzero = !(rem.is_empty() || rem.iter().all(|d| *d == 0));
                let sticky = root[keep + 1..].iter().any(|d| *d != 0) || rem_nonzero;
                let last_odd = kept.last().is_some_and(|d| d % 2 == 1);
                let mut bump = dropped as i32;
                if guard > 5 || (guard == 5 && (sticky || last_odd)) {
                    kept = add_mag(&kept, &[1]);
                    if kept.len() > keep {
                        kept.truncate(keep);
                        bump += 1;
                    }
                }
                (kept, f + bump)
            } else {
                (root.clone(), f)
            };

            // An EXACT result is expressed at the ideal exponent: strip trailing
            // zeros up to it, and pad back down to it when there is room.
            let (coeff, exp2) = if exact {
                let mut c = coeff;
                let mut e = exp2;
                while e < ideal && c.len() > 1 && *c.last().unwrap() == 0 {
                    c.pop();
                    e += 1;
                }
                while e > ideal && c.len() < MAX_DIGITS {
                    c.push(0);
                    e -= 1;
                }
                (c, e)
            } else {
                (coeff, exp2)
            };

            Some(Dec::Fin {
                sign: 1,
                coeff: strip_leading(&coeff).to_vec(),
                exp: exp2,
            })
        }
    }
}

#[cfg(test)]
mod sqrt_tests {
    use super::{parse, sqrt, to_string};

    /// Every expectation is mongod 8.2.11's own answer, measured 2026-09-08.
    /// The exact cases pin the IDEAL-EXPONENT rule -- `sqrt(4)` is `2`, not
    /// `2.000...` -- which is the half a naive 34-digit implementation gets
    /// wrong.
    #[test]
    fn matches_mongod() {
        let cases = [
            ("0", "0"),
            ("0.00", "0.0"),
            ("1", "1"),
            ("4", "2"),
            ("100", "10"),
            ("0.25", "0.5"),
            ("1E+10", "1E+5"),
            ("1E-10", "0.00001"),
            ("1E-6176", "1E-3088"),
            ("2", "1.414213562373095048801688724209698"),
            ("2.5", "1.581138830084189665999446772216359"),
            ("0.5", "0.7071067811865475244008443621048490"),
            ("1.25", "1.118033988749894848204586834365638"),
            ("3", "1.732050807568877293527446341505872"),
            ("7.125", "2.669269563007827802702880991398928"),
            ("0.001", "0.03162277660168379331998893544432719"),
            ("1E+6111", "3.162277660168379331998893544432719E+3055"),
            (
                "9.999999999999999999999999999999999E+6144",
                "3.162277660168379331998893544432718E+3072",
            ),
        ];
        for (input, want) in cases {
            let got = sqrt(&parse(input).unwrap()).unwrap();
            assert_eq!(to_string(&got), want, "sqrt({input})");
        }
    }

    /// A negative operand is out of domain; the caller turns `None` into
    /// mongod's 28714. `-0` is NOT negative here -- mongod answers `-0`.
    #[test]
    fn negative_is_out_of_domain() {
        assert!(sqrt(&parse("-1").unwrap()).is_none());
        assert!(sqrt(&parse("-2.5").unwrap()).is_none());
        assert!(sqrt(&parse("-Infinity").unwrap()).is_none());
        assert_eq!(to_string(&sqrt(&parse("-0").unwrap()).unwrap()), "-0");
    }

    #[test]
    fn nan_and_infinity_pass_through() {
        assert_eq!(to_string(&sqrt(&parse("NaN").unwrap()).unwrap()), "NaN");
        assert_eq!(
            to_string(&sqrt(&parse("Infinity").unwrap()).unwrap()),
            "Infinity"
        );
    }

    /// Squaring the result must reproduce the input whenever the root is exact.
    #[test]
    fn exact_roots_round_trip() {
        for n in ["1", "4", "9", "16", "100", "0.25", "0.0001", "625"] {
            let r = sqrt(&parse(n).unwrap()).unwrap();
            let sq = super::mul(&r, &r).unwrap();
            let (a, b) = (
                to_string(&sq).parse::<f64>().unwrap(),
                n.parse::<f64>().unwrap(),
            );
            assert!((a - b).abs() < 1e-12, "sqrt({n})^2 = {a}, want {b}");
        }
    }
}

/// `x` rounded to an INTEGER quantum, or `None` when the integral value needs
/// more than decimal128's 34 digits.
///
/// This is the decimal spec's `quantize`, and the `None` is its Invalid
/// Operation: `$floor` / `$ceil` of `Decimal128("1E+34")` is `NaN` on mongod,
/// not the value. `$trunc` / `$round` deliberately do NOT share it -- they
/// answer `1.000000000000000000000000000000000E+34` for the same input
/// (measured 8.2.11, 2026-09-07), so the rule belongs here and not in
/// `round_to_exp`.
pub fn quantize_integral(d: &Dec, mode: RoundMode) -> Option<Dec> {
    let Dec::Fin { coeff, exp, .. } = d else {
        return Some(d.clone());
    };
    // `adjusted` is the exponent of the leading digit, so `>= MAX_DIGITS` means
    // the value alone already needs 35 or more integer digits.
    let mag = strip_leading(coeff);
    if !mag.iter().all(|x| *x == 0) && *exp + mag.len() as i32 > MAX_DIGITS as i32 {
        return None;
    }
    let r = round_to_exp(d, 0, mode)?;
    // Rounding AWAY can carry into a 35th digit -- `$ceil` of
    // `9999999999999999999999999999999999.5` -- which overflows the same way.
    match &r {
        Dec::Fin { coeff, exp, .. } if *exp > 0 && !coeff.iter().all(|x| *x == 0) => None,
        _ => Some(r),
    }
}

// ---------------------------------------------------------------------------
// high-precision arithmetic, for the transcendentals
// ---------------------------------------------------------------------------
//
// `add` / `mul` above round every result to decimal128's 34 digits, which is
// correct for arithmetic and useless for a series: an argument reduction can
// cancel 30 digits away, and rounding at each step would leave nothing behind
// the decimal point. These helpers do the same arithmetic at a caller-chosen
// width and skip the decimal128 range rules entirely -- an intermediate is
// allowed to be far outside the format. Only the final `to_bson` clamps.
//
// None of them handle NaN or the infinities; every caller settles those first.

/// Round a raw `(sign, coeff, exp)` to `prec` digits. The high-precision twin
/// of `finish`, without its 34-digit cap.
fn hp_norm(sign: i8, coeff: Vec<u8>, exp: i32, prec: usize) -> Dec {
    let c = strip_leading(&coeff).to_vec();
    let (c, bump) = round_half_even(&c, prec);
    Dec::Fin {
        sign,
        coeff: c,
        exp: exp + bump,
    }
}

/// The exponent of a value's LEADING digit, or `None` for a zero.
fn hp_adjusted(d: &Dec) -> Option<i32> {
    let Dec::Fin { coeff, exp, .. } = d else {
        return None;
    };
    let mag = strip_leading(coeff);
    (!mag.iter().all(|x| *x == 0)).then(|| exp + mag.len() as i32 - 1)
}

fn hp_is_zero(d: &Dec) -> bool {
    matches!(d, Dec::Fin { coeff, .. } if coeff.iter().all(|x| *x == 0))
}

fn hp_from_i64(n: i64, exp: i32) -> Dec {
    let sign = if n < 0 { -1 } else { 1 };
    let digits: Vec<u8> = if n == 0 {
        vec![0]
    } else {
        n.unsigned_abs()
            .to_string()
            .bytes()
            .map(|b| b - b'0')
            .collect()
    };
    Dec::Fin {
        sign,
        coeff: digits,
        exp,
    }
}

fn hp_neg(d: &Dec) -> Dec {
    match d {
        Dec::Fin { sign, coeff, exp } => Dec::Fin {
            sign: -sign,
            coeff: coeff.clone(),
            exp: *exp,
        },
        other => other.clone(),
    }
}

fn hp_mul(a: &Dec, b: &Dec, prec: usize) -> Dec {
    let (
        Dec::Fin {
            sign: s1,
            coeff: c1,
            exp: e1,
        },
        Dec::Fin {
            sign: s2,
            coeff: c2,
            exp: e2,
        },
    ) = (a, b)
    else {
        return Dec::Nan;
    };
    hp_norm(s1 * s2, mul_mag(c1, c2), e1 + e2, prec)
}

fn hp_add(a: &Dec, b: &Dec, prec: usize) -> Dec {
    if hp_is_zero(a) {
        return b.clone();
    }
    if hp_is_zero(b) {
        return a.clone();
    }
    // An operand more than `prec` orders below the other cannot change any
    // digit that survives, and aligning it would build a vector that size.
    match (hp_adjusted(a), hp_adjusted(b)) {
        (Some(x), Some(y)) if x - y > prec as i32 + 2 => return a.clone(),
        (Some(x), Some(y)) if y - x > prec as i32 + 2 => return b.clone(),
        _ => {}
    }
    let (
        Dec::Fin {
            sign: s1,
            coeff: c1,
            exp: e1,
        },
        Dec::Fin {
            sign: s2,
            coeff: c2,
            exp: e2,
        },
    ) = (a, b)
    else {
        return Dec::Nan;
    };
    let target = (*e1).min(*e2);
    let x = scale_to(c1, *e1, target);
    let y = scale_to(c2, *e2, target);
    let (sign, mag) = if s1 == s2 {
        (*s1, add_mag(&x, &y))
    } else {
        match cmp_mag(&x, &y) {
            std::cmp::Ordering::Equal => return hp_from_i64(0, target),
            std::cmp::Ordering::Greater => (*s1, sub_mag(&x, &y)),
            std::cmp::Ordering::Less => (*s2, sub_mag(&y, &x)),
        }
    };
    hp_norm(sign, mag, target, prec)
}

fn hp_sub(a: &Dec, b: &Dec, prec: usize) -> Dec {
    hp_add(a, &hp_neg(b), prec)
}

/// `a / b` to `prec` digits, by schoolbook long division.
///
/// The quotient is TRUNCATED, not rounded, at `prec + 2` digits -- the two
/// extra are what keeps the caller's own rounding honest.
fn hp_div(a: &Dec, b: &Dec, prec: usize) -> Dec {
    let (
        Dec::Fin {
            sign: s1,
            coeff: c1,
            exp: e1,
        },
        Dec::Fin {
            sign: s2,
            coeff: c2,
            exp: e2,
        },
    ) = (a, b)
    else {
        return Dec::Nan;
    };
    let num = strip_leading(c1);
    let den = strip_leading(c2);
    if den.iter().all(|x| *x == 0) {
        return Dec::Nan;
    }
    if num.iter().all(|x| *x == 0) {
        return hp_from_i64(0, 0);
    }
    let want = prec + 2;
    // Produce `want` quotient digits starting at the first non-zero one.
    let mut rem: Vec<u8> = Vec::with_capacity(den.len() + 2);
    let mut quot: Vec<u8> = Vec::with_capacity(want + 1);
    let mut taken = 0usize; // digits of the numerator consumed
    let mut started = false;
    while quot.len() < want {
        // Bring down the next numerator digit (zero once it is exhausted).
        rem.push(if taken < num.len() { num[taken] } else { 0 });
        taken += 1;
        let r = strip_leading(&rem).to_vec();
        let mut q = 0u8;
        let mut cur = r;
        while cmp_mag(&cur, den) != std::cmp::Ordering::Less {
            cur = sub_mag(&cur, den);
            cur = strip_leading(&cur).to_vec();
            q += 1;
        }
        rem = cur;
        if q > 0 {
            started = true;
        }
        if started {
            quot.push(q);
        }
        // While `started` is false the remainder is still below the divisor, so
        // no significant digit has appeared: the exponent absorbs the position
        // instead, which `shift` below accounts for via `taken`.
        // The first significant quotient digit cannot appear until the running
        // remainder reaches the divisor, which takes one iteration per digit
        // the DIVISOR has beyond the numerator -- `ln(10)` carries 130 of them.
        // A bound that ignored `den.len()` aborted `x / ln10` at 90 iterations
        // and returned ZERO, which left `hp_exp`'s argument unreduced and made
        // the Taylor series run past its own term cap for any `x` over ~1000.
        if !started && taken > num.len() + den.len() + prec + 4 {
            return hp_from_i64(0, 0); // unreachable for a non-zero numerator
        }
    }
    // `taken` numerator digits produced `quot.len()` quotient digits, and the
    // first quotient digit sits at numerator position `taken - quot.len()`.
    let shift = e1 - e2 + num.len() as i32 - taken as i32;
    hp_norm(s1 * s2, quot, shift, prec)
}

/// `sqrt(a)` to `prec` digits, from the exact integer root.
fn hp_sqrt(a: &Dec, prec: usize) -> Dec {
    let Dec::Fin { coeff, exp, .. } = a else {
        return Dec::Nan;
    };
    let mag = strip_leading(coeff);
    if mag.iter().all(|x| *x == 0) {
        return hp_from_i64(0, 0);
    }
    // Pad so the integer root carries `prec + 2` digits, keeping the exponent
    // even so it halves cleanly.
    let want = 2 * (prec + 2);
    let mut k = want.saturating_sub(mag.len());
    if (exp - k as i32) % 2 != 0 {
        k += 1;
    }
    let mut n = mag.to_vec();
    n.extend(std::iter::repeat_n(0u8, k));
    let (root, _) = isqrt_mag(&n);
    hp_norm(1, root, (exp - k as i32) / 2, prec)
}

/// `ln(2)` and `ln(10)` to 130 digits -- more than any working precision here
/// asks for, so the reduction below adds no error of its own.
const LN2_TEXT: &str = "0.6931471805599453094172321214581765680755001343602552541206800094933936219696947156058633269964186875420014810205706857336855202358";
const LN10_TEXT: &str = "2.302585092994045684017991454684364207601101488628772976033327900967572609677352480235997205089598298341967784042286248633409525465";

/// `ln(x)` for a POSITIVE finite `x`, to `prec` digits.
///
/// `x = m * 10^k` with `m` in `[1, 10)`, then `m = r * 2^j` with `r` near 1, so
/// `ln(x) = 2*atanh(z) + j*ln2 + k*ln10` where `z = (r-1)/(r+1)`. The halvings
/// put `|z|` under 0.172, which the atanh series clears in about 45 terms at 70
/// digits -- the series is the only slow part and it converges geometrically.
fn hp_ln(x: &Dec, prec: usize) -> Option<Dec> {
    let Dec::Fin { coeff, exp, .. } = x else {
        return None;
    };
    let mag = strip_leading(coeff);
    if mag.iter().all(|d| *d == 0) {
        return None;
    }
    // m in [1, 10): the coefficient with the point after its first digit.
    let k = exp + mag.len() as i32 - 1;
    let mut r = Dec::Fin {
        sign: 1,
        coeff: mag.to_vec(),
        exp: -(mag.len() as i32 - 1),
    };
    // Halve until r is under sqrt(2); at most four times, since m < 10.
    let two = hp_from_i64(2, 0);
    let sqrt2_upper = hp_from_i64(14142136, -7); // slightly above sqrt(2)
    let mut j: i64 = 0;
    while cmp_abs(&r, &sqrt2_upper) == Some(std::cmp::Ordering::Greater) {
        r = hp_div(&r, &two, prec);
        j += 1;
    }
    let one = hp_from_i64(1, 0);
    let z = hp_div(&hp_sub(&r, &one, prec), &hp_add(&r, &one, prec), prec);
    // 2 * (z + z^3/3 + z^5/5 + ...)
    let z2 = hp_mul(&z, &z, prec);
    let mut term = z.clone();
    let mut acc = z.clone();
    let mut n: i64 = 1;
    loop {
        term = hp_mul(&term, &z2, prec);
        n += 2;
        let piece = hp_div(&term, &hp_from_i64(n, 0), prec);
        if hp_is_zero(&piece) {
            break;
        }
        // Once a term sits entirely below the working precision it cannot move
        // any digit, and every later term is smaller still.
        match (hp_adjusted(&acc), hp_adjusted(&piece)) {
            (Some(a), Some(p)) if a - p > prec as i32 + 2 => break,
            _ => {}
        }
        acc = hp_add(&acc, &piece, prec);
        if n > 4 * prec as i64 + 40 {
            return None; // the reduction failed to make z small; refuse
        }
    }
    let mut out = hp_mul(&acc, &two, prec);
    if j != 0 {
        let ln2 = parse(LN2_TEXT)?;
        out = hp_add(&out, &hp_mul(&hp_from_i64(j, 0), &ln2, prec), prec);
    }
    if k != 0 {
        let ln10 = parse(LN10_TEXT)?;
        out = hp_add(&out, &hp_mul(&hp_from_i64(k as i64, 0), &ln10, prec), prec);
    }
    Some(out)
}

/// `e^x` for a finite `x`, to `prec` digits.
///
/// `x = m*ln10 + r` with `|r| <= ln10/2`, so `e^x = e^r * 10^m` and the power of
/// ten costs nothing but an exponent. `r` is then halved four more times before
/// the Taylor series and the result squared back, which cuts the term count by
/// about half.
fn hp_exp(x: &Dec, prec: usize) -> Option<Dec> {
    if hp_is_zero(x) {
        return Some(hp_from_i64(1, 0));
    }
    let ln10 = parse(LN10_TEXT)?;
    // m = round(x / ln10), as an integer.
    let quo = hp_div(x, &ln10, prec);
    let m = hp_round_to_i64(&quo)?;
    let r = hp_sub(x, &hp_mul(&hp_from_i64(m, 0), &ln10, prec), prec);
    // Halve r four times; |r| <= ln10/2 so |r/16| <= 0.072.
    const HALVINGS: u32 = 4;
    let mut t = r;
    for _ in 0..HALVINGS {
        t = hp_div(&t, &hp_from_i64(2, 0), prec);
    }
    // sum t^n / n!
    let mut term = hp_from_i64(1, 0);
    let mut acc = hp_from_i64(1, 0);
    let mut n: i64 = 1;
    loop {
        term = hp_div(&hp_mul(&term, &t, prec), &hp_from_i64(n, 0), prec);
        if hp_is_zero(&term) {
            break;
        }
        match (hp_adjusted(&acc), hp_adjusted(&term)) {
            (Some(a), Some(p)) if a - p > prec as i32 + 2 => break,
            _ => {}
        }
        acc = hp_add(&acc, &term, prec);
        n += 1;
        if n > 4 * prec as i64 + 40 {
            return None;
        }
    }
    for _ in 0..HALVINGS {
        acc = hp_mul(&acc, &acc, prec);
    }
    // Multiply by 10^m -- an exponent shift, not an arithmetic operation.
    let Dec::Fin { sign, coeff, exp } = acc else {
        return None;
    };
    Some(Dec::Fin {
        sign,
        coeff,
        exp: exp.checked_add(i32::try_from(m).ok()?)?,
    })
}

/// A `Dec` known to be a modest integer-valued quantity, as `i64`.
fn hp_round_to_i64(d: &Dec) -> Option<i64> {
    let rounded = round_to_exp(d, 0, RoundMode::HalfEven)?;
    let Dec::Fin { sign, coeff, exp } = rounded else {
        return None;
    };
    if exp < 0 {
        return None;
    }
    let mut v: i64 = 0;
    for digit in coeff.iter().chain(std::iter::repeat_n(&0u8, exp as usize)) {
        v = v.checked_mul(10)?.checked_add(i64::from(*digit))?;
    }
    Some(v * i64::from(sign))
}

/// Round a high-precision result to decimal128's 34 digits, or `None` when the
/// guard digits cannot decide the rounding.
///
/// The series above are computed at a wide working precision and are accurate
/// to within a few units of its last digit. That pins the 34-digit answer
/// UNLESS the true value sits astride a rounding boundary -- digits 35 onward
/// reading `4999…` or `5000…` all the way into the guard. Then the guard says
/// nothing and the caller recomputes wider (Ziv's strategy) rather than
/// guessing.
fn hp_round_34(d: &Dec, prec: usize) -> Option<Dec> {
    let Dec::Fin { sign, coeff, exp } = d else {
        return Some(d.clone());
    };
    let mag = strip_leading(coeff);
    if mag.len() <= MAX_DIGITS {
        return Some(d.clone());
    }
    // The window between the last digit we keep and the last few the series
    // cannot vouch for.
    let tail = &mag[MAX_DIGITS..];
    let trust = tail.len().saturating_sub(6);
    if trust > 1 {
        let window = &tail[1..trust];
        let borderline = (tail[0] == 4 && window.iter().all(|d| *d == 9))
            || (tail[0] == 5 && window.iter().all(|d| *d == 0));
        if borderline {
            return None;
        }
    }
    let _ = prec;
    // `round_half_even`'s `bump` ALREADY carries the dropped-digit count (see
    // `finish`, which adds nothing else); adding it again moved every result up
    // by `prec - 34` orders.
    let (kept, bump) = round_half_even(mag, MAX_DIGITS);
    Some(Dec::Fin {
        sign: *sign,
        coeff: kept,
        exp: exp + bump,
    })
}

/// Run `f` at increasing working precision until the 34-digit rounding is
/// decided. Three attempts is far past what any measured value needed.
fn with_precision(mut f: impl FnMut(usize) -> Option<Dec>) -> Option<Dec> {
    for prec in [80usize, 140, 260] {
        let raw = f(prec)?;
        if let Some(r) = hp_round_34(&raw, prec) {
            return Some(r);
        }
    }
    None
}

/// `ln(x)` at decimal128 precision. `None` for a value outside the domain --
/// the caller raises mongod's error, which differs per operator.
pub fn ln(a: &Dec) -> Option<Dec> {
    match a {
        Dec::Nan => Some(Dec::Nan),
        Dec::Inf(1) => Some(Dec::Inf(1)),
        Dec::Inf(_) => None,
        Dec::Fin { sign, .. } if *sign < 0 || a.is_zero() => None,
        _ => with_precision(|p| hp_ln(a, p)),
    }
}

/// `e^x` at decimal128 precision.
pub fn exp(a: &Dec) -> Option<Dec> {
    match a {
        Dec::Nan => Some(Dec::Nan),
        Dec::Inf(1) => Some(Dec::Inf(1)),
        Dec::Inf(_) => Some(Dec::Fin {
            sign: 1,
            coeff: vec![0],
            exp: 0,
        }),
        _ => with_precision(|p| hp_exp(a, p)),
    }
}

/// `log10(x)` for a POSITIVE finite `x`, to `prec` digits.
///
/// `x = m * 10^k` with `m` in `[1, 10)`, so `log10(x) = k + ln(m)/ln(10)`. An
/// exact power of ten therefore answers the INTEGER `k` with no series at all,
/// which is the only way `log10(100)` comes out as `2` rather than
/// `1.999…`.
fn hp_log10(x: &Dec, prec: usize) -> Option<Dec> {
    let Dec::Fin { coeff, exp, .. } = x else {
        return None;
    };
    let mag = strip_leading(coeff);
    if mag.iter().all(|d| *d == 0) {
        return None;
    }
    let k = exp + mag.len() as i32 - 1;
    // A coefficient of a single 1 followed by zeros IS a power of ten.
    if mag[0] == 1 && mag[1..].iter().all(|d| *d == 0) {
        return Some(hp_from_i64(i64::from(k), 0));
    }
    let m = Dec::Fin {
        sign: 1,
        coeff: mag.to_vec(),
        exp: -(mag.len() as i32 - 1),
    };
    let ln10 = parse(LN10_TEXT)?;
    let frac = hp_div(&hp_ln(&m, prec)?, &ln10, prec);
    Some(hp_add(&hp_from_i64(i64::from(k), 0), &frac, prec))
}

/// `log10(x)` at decimal128 precision.
pub fn log10(a: &Dec) -> Option<Dec> {
    match a {
        Dec::Nan => Some(Dec::Nan),
        Dec::Inf(1) => Some(Dec::Inf(1)),
        Dec::Inf(_) => None,
        Dec::Fin { sign, .. } if *sign < 0 || a.is_zero() => None,
        _ => with_precision(|p| hp_log10(a, p)),
    }
}

/// `asinh(x) = ln(x + sqrt(x^2 + 1))`, at decimal128 precision.
///
/// An odd function, computed on `|x|` and signed back: for a NEGATIVE `x` the
/// sum `x + sqrt(x^2+1)` cancels to about `1/(2|x|)`, which throws away as many
/// digits as `x` has, and no working precision fixes that in general.
pub fn asinh(a: &Dec) -> Option<Dec> {
    match a {
        Dec::Nan => Some(Dec::Nan),
        Dec::Inf(s) => Some(Dec::Inf(*s)),
        Dec::Fin { sign, .. } => {
            if a.is_zero() {
                // Measured 8.2.11: `$asinh` of a decimal zero carries the sign
                // and the MINIMUM quantum, which no arithmetic here produces.
                return Some(Dec::Fin {
                    sign: *sign,
                    coeff: vec![0],
                    exp: -6176,
                });
            }
            let neg = *sign < 0;
            let x = if neg { hp_neg(a) } else { a.clone() };
            // `asinh(x) = x - x^3/6 + ...`, so once `|x|` is 18 or more orders
            // below 1 the correction sits at least 36 orders under the leading
            // digit -- past all 34 of them -- and the answer IS `x`, expressed
            // at 34 significant digits.
            //
            // This is not an optimisation. Any FIXED working precision loses a
            // tiny argument entirely: `1 + 1E-100` is `1` at 80 digits, so
            // `asinh(Decimal128("1E-100"))` came back `0` where mongod and the
            // Python engine both answer `1.000000000000000000000000000000000E-100`.
            // Scaling the precision with the exponent instead would mean 6000-digit
            // series arithmetic at the bottom of the format.
            if let Some(adj) = hp_adjusted(&x) {
                if adj <= -18 {
                    let Dec::Fin { coeff, exp, .. } = &x else {
                        return None;
                    };
                    let mag = strip_leading(coeff);
                    let mut c = mag.to_vec();
                    let pad = MAX_DIGITS.saturating_sub(c.len());
                    c.extend(std::iter::repeat_n(0u8, pad));
                    return Some(Dec::Fin {
                        sign: if neg { -1 } else { 1 },
                        coeff: c,
                        exp: exp - pad as i32,
                    });
                }
            }
            let out = with_precision(|p| {
                let x2 = hp_mul(&x, &x, p);
                let root = hp_sqrt(&hp_add(&x2, &hp_from_i64(1, 0), p), p);
                hp_ln(&hp_add(&x, &root, p), p)
            })?;
            Some(if neg { hp_neg(&out) } else { out })
        }
    }
}

#[cfg(test)]
mod transcendental_tests {
    use super::*;

    fn s(d: Option<Dec>) -> String {
        d.map(|x| to_string(&x)).unwrap_or_else(|| "<none>".into())
    }

    /// The CORRECTLY-ROUNDED 34-digit values, computed independently at 250
    /// digits and stable at 60 / 120 / 200.
    ///
    /// These are NOT all mongod's answers. Over 290 measured pairs mongod is
    /// correctly rounded on 231 -- it carries Intel RDFP's approximation error
    /// in the last digit on the rest -- so six expectations here differ from
    /// 8.2.11 by one unit in the last place. That divergence is deliberate and
    /// was asked for; see `tasks/backlog.md`.
    #[test]
    fn ln_is_correctly_rounded() {
        for (input, want) in [
            ("1", "0"),
            ("2", "0.6931471805599453094172321214581766"),
            ("10", "2.302585092994045684017991454684364"),
            ("2.5", "0.9162907318741550651835272117680111"),
            ("0.5", "-0.6931471805599453094172321214581766"),
            ("100", "4.605170185988091368035982909368728"),
            ("1E+400", "921.0340371976182736071965818737457"),
        ] {
            let got = s(ln(&parse(input).unwrap()));
            assert_eq!(got, want, "ln({input})");
        }
    }

    #[test]
    fn exp_is_correctly_rounded() {
        for (input, want) in [
            ("0", "1"),
            ("1", "2.718281828459045235360287471352662"),
            ("2.5", "12.18249396070347343807017595116797"),
            ("-1", "0.3678794411714423215955237701614609"),
            ("180", "1.489384200781838359564441023032289E+78"),
        ] {
            let got = s(exp(&parse(input).unwrap()));
            assert_eq!(got, want, "exp({input})");
        }
    }

    #[test]
    fn asinh_is_correctly_rounded() {
        for (input, want) in [
            ("1", "0.8813735870195430252326093249797923"),
            ("-1", "-0.8813735870195430252326093249797923"),
            ("2", "1.443635475178810342493276740273105"),
            ("2.5", "1.647231146371095710624858610443620"),
            ("10", "2.998222950297969738846595537596453"),
            ("0.5", "0.4812118250596034474977589134243684"),
            ("0.1", "0.09983407889920756332730312470476944"),
            ("100", "5.298342365610588757368825689112906"),
            ("3", "1.818446459232066823483698963560709"),
            ("7.125", "2.661645514507905051660213687863223"),
            ("0.001", "0.0009999998333334083332886905065723983"),
            ("1E+10", "23.71899811050040214959964666830182"),
            ("1E-10", "9.999999999999999999983333333333333E-11"),
            ("123456789.987654321", "19.32454895472796339991366575636355"),
            ("1E+34", "78.98104034235749856602894158072656"),
            ("1E+400", "921.7271843781782189166138139952039"),
            ("1E+310", "714.4945260087141073549945830736111"),
            ("1E+6144", "14147.77595853597662791595672970219"),
            ("-1E+6144", "-14147.77595853597662791595672970219"),
            // Below 1E-18 the answer is the argument at 34 digits. A fixed
            // working precision returned 0 for every one of these.
            ("1E-17", "1.000000000000000000000000000000000E-17"),
            ("1E-20", "1.000000000000000000000000000000000E-20"),
            ("1E-34", "1.000000000000000000000000000000000E-34"),
            ("1E-100", "1.000000000000000000000000000000000E-100"),
            ("1E-3000", "1.000000000000000000000000000000000E-3000"),
            ("-1E-100", "-1.000000000000000000000000000000000E-100"),
            // Just ABOVE the shortcut, where the series must still run.
            ("1E-15", "9.999999999999999999999999999998333E-16"),
            ("0.9", "0.8088669356527824625093501673816060"),
            ("1.1", "0.9503469298211342502700715942698944"),
            ("-2.5", "-1.647231146371095710624858610443620"),
            ("-100", "-5.298342365610588757368825689112906"),
        ] {
            let got = s(asinh(&parse(input).unwrap()));
            assert_eq!(got, want, "asinh({input})");
        }
    }

    #[test]
    fn log10_is_correctly_rounded() {
        for (input, want) in [
            // Exact powers of ten answer the integer, with no series.
            ("1", "0"),
            ("10", "1"),
            ("100", "2"),
            ("1E+400", "400"),
            ("0.001", "-3"),
            ("2", "0.3010299956639811952137388947244930"),
            ("2.5", "0.3979400086720376095725222105510139"),
            ("7.125", "0.8527848686805478131901446948385655"),
        ] {
            let got = s(log10(&parse(input).unwrap()));
            assert_eq!(got, want, "log10({input})");
        }
    }

    /// `hp_div` against a divisor far wider than the dividend -- the shape
    /// that silently returned zero.
    #[test]
    fn division_by_a_much_wider_divisor() {
        let x = parse("4920.26").unwrap();
        let ln10 = parse(LN10_TEXT).unwrap();
        let q = hp_div(&x, &ln10, 80);
        // 4920.26 / ln(10) = 2136.844...
        assert!(
            to_string(&q).starts_with("2136.84"),
            "quotient was {}",
            to_string(&q)
        );
        // And a plain one, to pin the exponent bookkeeping.
        assert!(
            to_string(&hp_div(&parse("1").unwrap(), &parse("3").unwrap(), 20))
                .starts_with("0.3333333333")
        );
    }

    #[test]
    fn specials_pass_through() {
        assert_eq!(s(asinh(&Dec::Nan)), "NaN");
        assert_eq!(s(asinh(&Dec::Inf(1))), "Infinity");
        assert_eq!(s(asinh(&Dec::Inf(-1))), "-Infinity");
        assert_eq!(s(ln(&Dec::Inf(1))), "Infinity");
        assert!(ln(&parse("0").unwrap()).is_none());
        assert!(ln(&parse("-1").unwrap()).is_none());
    }
}

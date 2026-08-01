//! Numeric-type bridging for query matching — the int32 / int64 / double /
//! Decimal128 "single numeric type" semantics MongoDB uses for equality and
//! comparison (`secantus.query._eq_numeric_aware` / `_coerce_numeric`).
//!
//! Values are reduced to a sign + normalised decimal-digit form so that, e.g.,
//! int `5`, double `5.0`, and `Decimal128("5")` compare equal, and ordering is
//! correct across the unified numeric type. NaN and ±Infinity get dedicated
//! variants with MongoDB's ordering (−Inf < finite < +Inf; NaN is unordered).
//! `bool` is deliberately *not* numeric here — the matcher handles bool
//! separately so it stays a distinct BSON type.

use std::cmp::Ordering;

use bson::Bson;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NumVal {
    Nan,
    Inf(i8), // +1 / -1
    Zero,
    Finite { sign: i8, digits: Vec<u8>, exp: i64 },
}

/// Classify a BSON value as numeric, or `None` if it isn't a number (incl.
/// bool, which is intentionally excluded).
pub fn classify(b: &Bson) -> Option<NumVal> {
    match b {
        Bson::Int32(n) => Some(from_int(*n as i128)),
        Bson::Int64(n) => Some(from_int(*n as i128)),
        Bson::Double(d) => Some(from_f64(*d)),
        Bson::Decimal128(d) => Some(from_decimal_str(&d.to_string())),
        _ => None,
    }
}

/// Integer value of an int32/int64/bool (bool as 0/1, since Python's `int` is a
/// superclass of `bool`). `None` for non-integer-like BSON.
pub fn as_int_like(b: &Bson) -> Option<i128> {
    match b {
        Bson::Int32(n) => Some(*n as i128),
        Bson::Int64(n) => Some(*n as i128),
        Bson::Boolean(v) => Some(i128::from(*v)),
        _ => None,
    }
}

/// Float value of any numberish BSON (double/int/bool). `None` otherwise.
pub fn as_float_like(b: &Bson) -> Option<f64> {
    match b {
        Bson::Double(d) => Some(*d),
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Boolean(v) => Some(if *v { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Encode an integer result with the BSON width pymongo would pick by
/// magnitude: int32 if it fits, else int64. `None` for > int64 (Python keeps a
/// big int that pymongo can't encode — caller should defer to Python).
pub fn int_to_bson(r: i128) -> Option<Bson> {
    if (i32::MIN as i128..=i32::MAX as i128).contains(&r) {
        Some(Bson::Int32(r as i32))
    } else if (i64::MIN as i128..=i64::MAX as i128).contains(&r) {
        Some(Bson::Int64(r as i64))
    } else {
        None
    }
}

/// Whether a BSON value is specifically a 64-bit int — the flag that drives
/// arithmetic type promotion (see [`int_promoted_to_bson`]).
pub fn is_int64(b: &Bson) -> bool {
    matches!(b, Bson::Int64(_))
}

/// Encode an integral arithmetic result honouring MongoDB's numeric promotion:
/// the result is **int64** if any operand was already int64 (`operand_wide`) or
/// a 32-bit result would overflow, otherwise **int32**. `None` for a result that
/// overflows int64 (Python keeps a big int pymongo can't encode — defer).
/// Mirrors the integral branch of `secantus.numerics._combine`.
pub fn int_promoted_to_bson(r: i128, operand_wide: bool) -> Option<Bson> {
    if !(i64::MIN as i128..=i64::MAX as i128).contains(&r) {
        return None;
    }
    let fits_i32 = (i32::MIN as i128..=i32::MAX as i128).contains(&r);
    if operand_wide || !fits_i32 {
        Some(Bson::Int64(r as i64))
    } else {
        Some(Bson::Int32(r as i32))
    }
}

fn normalize(digits: &mut Vec<u8>, exp: &mut i64) {
    while digits.first() == Some(&0) {
        digits.remove(0);
    }
    while digits.last() == Some(&0) {
        digits.pop();
        *exp += 1;
    }
}

/// `NumVal` for a signed integer (exposed for the densify cursor).
pub fn from_int(n: i128) -> NumVal {
    if n == 0 {
        return NumVal::Zero;
    }
    let sign = if n < 0 { -1 } else { 1 };
    let mag = if n < 0 { -n } else { n };
    let mut digits: Vec<u8> = mag.to_string().bytes().map(|b| b - b'0').collect();
    let mut exp = 0i64;
    normalize(&mut digits, &mut exp);
    NumVal::Finite { sign, digits, exp }
}

/// `NumVal` for an f64 (exposed for the densify cursor).
pub fn from_f64(d: f64) -> NumVal {
    if d.is_nan() {
        return NumVal::Nan;
    }
    if d.is_infinite() {
        return NumVal::Inf(if d > 0.0 { 1 } else { -1 });
    }
    if d == 0.0 {
        return NumVal::Zero;
    }
    // Rust's shortest Display matches Python's Decimal(repr(d)) digits.
    from_decimal_str(&format!("{d}"))
}

/// Exact ordering of an i64-range integer against a double, without
/// allocating. `None` = unordered (NaN). Exactness argument: when
/// `|f| < 2^63`, `f.trunc()` fits in i64 and converts exactly (any double
/// with `|f| > 2^53` is already integral, and integral doubles below 2^63
/// convert exactly); comparing the integer parts and tie-breaking on the
/// fractional remainder is then exact for every case, which is what the
/// digit-form [`cmp`] computes for the same pair.
fn cmp_i64_f64(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if f == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // `as` saturates: for |f| >= 2^63 (reachable via the ungated Int32 arms)
    // t pins to i64::MAX/MIN, which still orders correctly against any i32,
    // and the equal/frac tie-break is unreachable there (t != i). For
    // |f| < 2^63 the trunc converts exactly (integral doubles > 2^53 are
    // exact; below 2^53 everything is).
    let t = f.trunc() as i64;
    Some(match i.cmp(&t) {
        Ordering::Equal => {
            let frac = f - t as f64; // exact: t as f64 == trunc(f) exactly here
            if frac > 0.0 {
                Ordering::Less
            } else if frac < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        other => other,
    })
}

/// Allocation-free comparison fast path for the common int32/int64/double
/// pairs — byte-for-byte the same verdicts as `classify` + [`cmp`], skipping
/// the digit-vector build those allocate per call. Returns:
///
/// * `None`        — not applicable (Decimal128 or non-numeric involved);
///   the caller falls back to `classify`.
/// * `Some(None)`  — unordered (NaN involved), matching [`cmp`]'s `None`.
/// * `Some(Some)`  — the ordering.
///
/// Double↔double uses `partial_cmp` (shortest-repr digit ordering and f64
/// ordering agree for all finite doubles: repr round-trips, and rounding is
/// monotone, so distinct doubles keep their order and equal reprs mean equal
/// doubles; ±0.0 both classify to `Zero` and compare equal either way;
/// infinities and NaN match the variant handling).
///
/// The MIXED int↔double arm is gated to |values| ≤ 2^53. The engines'
/// established semantic (Python parity) compares a double by the decimal
/// value of its SHORTEST REPR, which can differ from the exact binary value
/// above 2^53 (e.g. the double 2^63 reprs as 9223372036854776000) — an exact
/// comparison would diverge there, so those pairs fall back to the digit
/// form. Within ±2^53 every integer in play is exactly representable and a
/// repr cannot cross an integer boundary, so exact and repr-decimal verdicts
/// coincide.
pub fn fast_cmp(a: &Bson, b: &Bson) -> Option<Option<Ordering>> {
    // Gate on the INTEGER only: an exactly-representable i (|i| <= 2^53)
    // rounds to itself, so it can never sit strictly between a double and
    // that double's repr-decimal — the two verdicts coincide for any double
    // operand. Int64 outside +/-2^53 declines to the digit form.
    const SAFE_I: i64 = 1 << 53;
    let mixed_ok = |i: i64| (-SAFE_I..=SAFE_I).contains(&i);
    match (a, b) {
        (Bson::Int32(x), Bson::Int32(y)) => Some(Some(x.cmp(y))),
        (Bson::Int64(x), Bson::Int64(y)) => Some(Some(x.cmp(y))),
        (Bson::Int32(x), Bson::Int64(y)) => Some(Some((*x as i64).cmp(y))),
        (Bson::Int64(x), Bson::Int32(y)) => Some(Some(x.cmp(&(*y as i64)))),
        (Bson::Double(x), Bson::Double(y)) => Some(x.partial_cmp(y)),
        (Bson::Int32(x), Bson::Double(y)) => Some(cmp_i64_f64(*x as i64, *y)),
        (Bson::Int64(x), Bson::Double(y)) if mixed_ok(*x) => Some(cmp_i64_f64(*x, *y)),
        (Bson::Double(x), Bson::Int32(y)) => {
            Some(cmp_i64_f64(*y as i64, *x).map(Ordering::reverse))
        }
        (Bson::Double(x), Bson::Int64(y)) if mixed_ok(*y) => {
            Some(cmp_i64_f64(*y, *x).map(Ordering::reverse))
        }
        _ => None,
    }
}

/// Equality companion to [`fast_cmp`] (NaN never equal, matching [`eq`]).
pub fn fast_eq(a: &Bson, b: &Bson) -> Option<bool> {
    fast_cmp(a, b).map(|o| o == Some(Ordering::Equal))
}

/// Whether a value is "numberish" in the expression engine's sense
/// (int/long/double/bool) — a type test only, no allocation.
pub fn is_numberish(b: &Bson) -> bool {
    matches!(
        b,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_)
    )
}

/// [`fast_cmp`] with the expression engine's numberish semantics: bool
/// compares as 0/1 (mirroring `as_num`). Same return contract as
/// [`fast_cmp`].
pub fn fast_cmp_numberish(a: &Bson, b: &Bson) -> Option<Option<Ordering>> {
    let widen = |v: &Bson| -> Option<Bson> {
        match v {
            Bson::Boolean(x) => Some(Bson::Int32(i32::from(*x))),
            _ => None,
        }
    };
    match (widen(a), widen(b)) {
        (None, None) => fast_cmp(a, b),
        (Some(wa), None) => fast_cmp(&wa, b),
        (None, Some(wb)) => fast_cmp(a, &wb),
        (Some(wa), Some(wb)) => fast_cmp(&wa, &wb),
    }
}

fn from_decimal_str(s: &str) -> NumVal {
    let low = s.to_lowercase();
    if low.contains("nan") {
        return NumVal::Nan;
    }
    if low.contains("inf") {
        return NumVal::Inf(if s.trim_start().starts_with('-') {
            -1
        } else {
            1
        });
    }
    let s = s.trim();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i8, r),
        None => (1i8, s.strip_prefix('+').unwrap_or(s)),
    };
    let (mantissa, exp_extra) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().unwrap_or(0)),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    let mut digits: Vec<u8> = Vec::new();
    for c in int_part.chars().chain(frac_part.chars()) {
        if let Some(d) = c.to_digit(10) {
            digits.push(d as u8);
        }
    }
    let mut exp = exp_extra - frac_part.len() as i64;
    normalize(&mut digits, &mut exp);
    if digits.is_empty() {
        NumVal::Zero
    } else {
        NumVal::Finite { sign, digits, exp }
    }
}

/// Numeric equality across the unified type (NaN never equal).
pub fn eq(a: &NumVal, b: &NumVal) -> bool {
    use NumVal::*;
    match (a, b) {
        (Nan, _) | (_, Nan) => false,
        (Inf(x), Inf(y)) => x == y,
        (Inf(_), _) | (_, Inf(_)) => false,
        (Zero, Zero) => true,
        (Zero, Finite { .. }) | (Finite { .. }, Zero) => false,
        (
            Finite {
                sign: s1,
                digits: d1,
                exp: e1,
            },
            Finite {
                sign: s2,
                digits: d2,
                exp: e2,
            },
        ) => s1 == s2 && d1 == d2 && e1 == e2,
    }
}

/// Magnitude comparison of two finite values (sign ignored).
fn cmp_magnitude(d1: &[u8], e1: i64, d2: &[u8], e2: i64) -> Ordering {
    let sci1 = e1 + d1.len() as i64 - 1;
    let sci2 = e2 + d2.len() as i64 - 1;
    match sci1.cmp(&sci2) {
        Ordering::Equal => d1.cmp(d2), // lexicographic; shorter prefix sorts first
        other => other,
    }
}

/// Numeric ordering; `None` when unordered (NaN involved), matching Python's
/// comparison returning False for any operator against NaN.
pub fn cmp(a: &NumVal, b: &NumVal) -> Option<Ordering> {
    use NumVal::*;
    Some(match (a, b) {
        (Nan, _) | (_, Nan) => return None,
        (Inf(x), Inf(y)) => x.cmp(y),
        (Inf(1), _) => Ordering::Greater,
        (_, Inf(1)) => Ordering::Less,
        (Inf(_), _) => Ordering::Less, // -Inf
        (_, Inf(_)) => Ordering::Greater,
        (Zero, Zero) => Ordering::Equal,
        (Zero, Finite { sign, .. }) => {
            if *sign > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Finite { sign, .. }, Zero) => {
            if *sign > 0 {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (
            Finite {
                sign: s1,
                digits: d1,
                exp: e1,
            },
            Finite {
                sign: s2,
                digits: d2,
                exp: e2,
            },
        ) => {
            if s1 != s2 {
                s1.cmp(s2)
            } else {
                let mag = cmp_magnitude(d1, *e1, d2, *e2);
                if *s1 > 0 {
                    mag
                } else {
                    mag.reverse()
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::Bson;

    fn n(b: Bson) -> NumVal {
        classify(&b).unwrap()
    }

    #[test]
    fn cross_type_equality() {
        assert!(eq(&n(Bson::Int32(5)), &n(Bson::Double(5.0))));
        assert!(eq(
            &n(Bson::Int32(5)),
            &n(Bson::Decimal128("5".parse().unwrap()))
        ));
        assert!(eq(
            &n(Bson::Double(3.5)),
            &n(Bson::Decimal128("3.5".parse().unwrap()))
        ));
        assert!(!eq(&n(Bson::Int32(5)), &n(Bson::Int32(6))));
    }

    #[test]
    fn ordering() {
        assert_eq!(
            cmp(&n(Bson::Int32(2)), &n(Bson::Double(3.5))),
            Some(Ordering::Less)
        );
        assert_eq!(
            cmp(
                &n(Bson::Decimal128("3.5".parse().unwrap())),
                &n(Bson::Int32(2))
            ),
            Some(Ordering::Greater)
        );
        assert_eq!(
            cmp(&n(Bson::Int32(-1)), &n(Bson::Int32(0))),
            Some(Ordering::Less)
        );
        assert_eq!(cmp(&n(Bson::Double(f64::NAN)), &n(Bson::Int32(0))), None);
    }
}

#[cfg(test)]
mod fast_path_tests {
    use super::*;

    /// Every int32/int64/double pair must get the same verdict from
    /// `fast_cmp` as from the digit-form `classify` + `cmp` it bypasses.
    #[test]
    fn fast_cmp_matches_classify_on_edge_corpus() {
        let ints: Vec<i64> = vec![
            0,
            1,
            -1,
            2,
            42,
            -42,
            (1 << 53) - 1,
            1 << 53,
            (1 << 53) + 1,
            -(1 << 53) - 1,
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MIN + 1,
            i32::MAX as i64,
            i32::MIN as i64,
        ];
        let doubles: Vec<f64> = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            -0.5,
            1.5,
            2.5e-10,
            9.007199254740992e15,  // 2^53
            9.007199254740993e15,  // rounds to 2^53
            9.223372036854776e18,  // 2^63 (shortest repr)
            -9.223372036854776e18, // -2^63 (shortest repr)
            9.3e18,
            -9.3e18,
            1e300,
            -1e300,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MIN_POSITIVE,
            42.0,
            #[allow(clippy::excessive_precision)]
            41.999_999_999_999_996, // nearest double just below 42
            42.00000000000001,
            (i64::MAX as f64),
            (i64::MIN as f64),
        ];
        let mut values: Vec<Bson> = Vec::new();
        for &i in &ints {
            if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                values.push(Bson::Int32(i as i32));
            }
            values.push(Bson::Int64(i));
        }
        for &d in &doubles {
            values.push(Bson::Double(d));
        }
        for a in &values {
            for b in &values {
                let slow = match (classify(a), classify(b)) {
                    (Some(na), Some(nb)) => Some(cmp(&na, &nb)),
                    _ => None,
                };
                // A `None` decline is always legal (the caller falls back to
                // the digit form); a `Some` answer must match it exactly.
                if let Some(fast) = fast_cmp(a, b) {
                    assert_eq!(
                        Some(fast),
                        slow,
                        "fast_cmp({a:?}, {b:?}) answered {fast:?} but the digit form said {slow:?}"
                    );
                }
                if let Some(fe) = fast_eq(a, b) {
                    let slow_eq = match (classify(a), classify(b)) {
                        (Some(na), Some(nb)) => eq(&na, &nb),
                        _ => unreachable!("fast_eq answered on a non-numeric pair"),
                    };
                    assert_eq!(fe, slow_eq, "fast_eq({a:?}, {b:?})");
                }
            }
        }
    }

    /// Decimal128 and non-numeric operands must be declined (caller falls
    /// back to the digit form), never mis-answered.
    #[test]
    fn fast_cmp_declines_decimal_and_non_numeric() {
        let dec: Bson = Bson::Decimal128("1.5".parse().unwrap());
        assert_eq!(fast_cmp(&dec, &Bson::Int32(1)), None);
        assert_eq!(fast_cmp(&Bson::Int32(1), &dec), None);
        assert_eq!(fast_cmp(&Bson::String("x".into()), &Bson::Int32(1)), None);
        assert_eq!(fast_cmp(&Bson::Boolean(true), &Bson::Int32(1)), None);
        // ...but the numberish variant maps bool to 0/1:
        assert_eq!(
            fast_cmp_numberish(&Bson::Boolean(true), &Bson::Int32(1)),
            Some(Some(std::cmp::Ordering::Equal))
        );
        assert_eq!(fast_cmp_numberish(&dec, &Bson::Int32(1)), None);
    }
}

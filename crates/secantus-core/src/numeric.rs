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

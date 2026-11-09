//! The Decimal128 arithmetic mongod's KeyString decoder does when it rebuilds
//! a decimal from a double (RM-075): Intel RDFP's `binary64_to_bid128` in a
//! directed rounding mode, `Decimal128`'s 15-digit constructor, `multiply`,
//! `normalize` and the continuation `add` (`src/mongo/platform/
//! decimal128.cpp`, `key_string.cpp`, mongod 8.2.11).
//!
//! Every value the decoder builds is rounded on its MAGNITUDE (it negates
//! after, or rounds away from zero when it negated first), so a value here
//! is a positive coefficient and an exponent: `coeff * 10^exp`.

/// decimal128's largest coefficient, 34 nines.
pub(super) const MAX_COEFF: u128 = 10u128.pow(34) - 1;

/// decimal128's smallest exponent (biased exponent 0).
pub(super) const MIN_EXP: i32 = -6176;

/// A finite, non-negative decimal128: `coeff * 10^exp`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Dec {
    pub coeff: u128,
    pub exp: i32,
}

/// The two directed roundings the decoder asks for, on a magnitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Round {
    /// Away from zero (`kRoundTowardPositive` on a positive value).
    Up,
    /// `kRoundTowardZero`.
    Down,
}

fn to_u128(digits: &[u8]) -> u128 {
    digits.iter().fold(0, |a, d| a * 10 + u128::from(*d))
}

fn to_digits(mut n: u128) -> Vec<u8> {
    let mut out = Vec::new();
    while n > 0 {
        out.push((n % 10) as u8);
        n /= 10;
    }
    out.reverse();
    out
}

fn digit_count(n: u128) -> u32 {
    n.checked_ilog10().map_or(1, |d| d + 1)
}

/// An exact value (`digits`, most significant first, no leading zeros, at
/// exponent `exp`) rounded to 34 digits.
fn round34(digits: &[u8], exp: i32, mode: Round) -> Dec {
    if digits.len() <= 34 {
        return Dec {
            coeff: to_u128(digits),
            exp,
        };
    }
    let mut coeff = to_u128(&digits[..34]);
    let mut exp = exp + (digits.len() - 34) as i32;
    if mode == Round::Up && digits[34..].iter().any(|d| *d != 0) {
        coeff += 1;
        if coeff > MAX_COEFF {
            // 99...9 + 1: the 35th digit is a zero, so this is exact.
            coeff /= 10;
            exp += 1;
        }
    }
    Dec { coeff, exp }
}

/// `binary64_to_bid128(d)` (`Decimal128(d, kRoundTo34Digits, mode)`) for a
/// finite `d >= 0`. An exact value keeps its shortest coefficient down to
/// exponent 0 (`0.5` is `5E-1`, `1e10` is `10000000000`); an inexact one
/// fills 34 digits.
pub(super) fn from_f64(d: f64, mode: Round) -> Dec {
    if d == 0.0 {
        return Dec { coeff: 0, exp: 0 };
    }
    // 767 significant digits write any f64 exactly.
    let s = format!("{d:.766e}");
    let (mantissa, e10) = s.split_once('e').unwrap_or((&s, "0"));
    let e10: i32 = e10.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let mut exp = e10 - 766;
    while digits.len() > 1 && digits.last() == Some(&0) && exp < 0 {
        digits.pop();
        exp += 1;
    }
    round34(&digits, exp, mode)
}

/// `bid128_quantize(x, 1E<target>, mode)` for a result that fits.
fn quantize(x: Dec, target: i32, mode: Round) -> Dec {
    if x.exp >= target {
        let up = (x.exp - target) as u32;
        return Dec {
            coeff: x.coeff.saturating_mul(10u128.saturating_pow(up)),
            exp: target,
        };
    }
    let down = (target - x.exp) as u32;
    let (q, rem) = match 10u128.checked_pow(down) {
        Some(p) => (x.coeff / p, x.coeff % p),
        None => (0, x.coeff),
    };
    Dec {
        coeff: if mode == Round::Up && rem != 0 {
            q + 1
        } else {
            q
        },
        exp: target,
    }
}

/// `floor(log2(d))` for a finite `d > 0`: `frexp`'s exponent less one.
fn floor_log2(d: f64) -> i32 {
    let bits = d.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    if biased == 0 {
        let mantissa = bits & ((1 << 52) - 1);
        (63 - mantissa.leading_zeros() as i32) - 1074
    } else {
        biased - 1023
    }
}

/// `Decimal128(d, kRoundTo15Digits, mode)` for a finite `d >= 0`: the
/// 34-digit conversion quantized to 15 significant digits in the same mode.
pub(super) fn from_f64_15(d: f64, mode: Round) -> Dec {
    let converted = from_f64(d, mode);
    if d == 0.0 {
        return converted;
    }
    let base2 = floor_log2(d);
    // mongod's lower bound of log10: C++ division truncates toward zero.
    let mut base10 = (base2 * 30103) / (100 * 1000);
    if base2 < 0 {
        base10 -= 1;
    }
    let q = quantize(converted, base10 - 14, mode);
    if q.coeff > 999_999_999_999_999 {
        return quantize(converted, base10 - 13, mode);
    }
    q
}

/// `a.multiply(b, mode)`: the exact product, rounded to 34 digits.
pub(super) fn multiply(a: Dec, b: Dec, mode: Round) -> Dec {
    let (x, y) = (to_digits(a.coeff), to_digits(b.coeff));
    if x.is_empty() || y.is_empty() {
        return Dec {
            coeff: 0,
            exp: a.exp + b.exp,
        };
    }
    let mut acc = vec![0u32; x.len() + y.len()];
    for (i, dx) in x.iter().enumerate().rev() {
        for (j, dy) in y.iter().enumerate().rev() {
            acc[i + j + 1] += u32::from(*dx) * u32::from(*dy);
        }
    }
    for k in (1..acc.len()).rev() {
        let carry = acc[k] / 10;
        acc[k] %= 10;
        acc[k - 1] += carry;
    }
    let digits: Vec<u8> = acc
        .iter()
        .map(|d| *d as u8)
        .skip_while(|d| *d == 0)
        .collect();
    round34(&digits, a.exp + b.exp, mode)
}

/// `normalize()`: `x + 0E-6176`, which widens the coefficient to 34 digits
/// (or as far as the smallest exponent allows).
pub(super) fn normalize(x: Dec) -> Dec {
    if x.coeff == 0 {
        return Dec {
            coeff: 0,
            exp: MIN_EXP,
        };
    }
    let room = 34 - digit_count(x.coeff) as i32;
    let shift = room.min(x.exp - MIN_EXP).max(0);
    Dec {
        coeff: x.coeff * 10u128.pow(shift as u32),
        exp: x.exp - shift,
    }
}

/// The continuation `add` of `readDecimalContinuation`: `x` (normalized)
/// plus `continuation` at its exponent, rounded half-even to 34 digits.
/// `None` when that rounding is inexact (mongod's 50815).
pub(super) fn add_continuation(x: Dec, continuation: u64) -> Option<Dec> {
    let sum = x.coeff + u128::from(continuation);
    if sum <= MAX_COEFF {
        return Some(Dec {
            coeff: sum,
            exp: x.exp,
        });
    }
    // At most 35 digits: exact only when the dropped digit is 0.
    sum.is_multiple_of(10).then_some(Dec {
        coeff: sum / 10,
        exp: x.exp + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_converts_as_intel_rdfp_does() {
        assert_eq!(from_f64(0.5, Round::Up), Dec { coeff: 5, exp: -1 });
        assert_eq!(
            from_f64(1e10, Round::Up),
            Dec {
                coeff: 10_000_000_000,
                exp: 0
            }
        );
        // 0.1 is 0.1000000000000000055511151231257827021181583404541015625.
        assert_eq!(
            from_f64(0.1, Round::Down),
            Dec {
                coeff: 1_000_000_000_000_000_055_511_151_231_257_827,
                exp: -34
            }
        );
        assert_eq!(
            from_f64(0.1, Round::Up),
            Dec {
                coeff: 1_000_000_000_000_000_055_511_151_231_257_828,
                exp: -34
            }
        );
    }

    #[test]
    fn the_fifteen_digit_conversion_rounds_up() {
        assert_eq!(
            from_f64_15(0.1, Round::Up),
            Dec {
                coeff: 100_000_000_000_001,
                exp: -15
            }
        );
        assert_eq!(
            from_f64_15(0.5, Round::Up),
            Dec {
                coeff: 500_000_000_000_000,
                exp: -15
            }
        );
    }

    #[test]
    fn normalize_fills_34_digits() {
        assert_eq!(
            normalize(Dec { coeff: 5, exp: -1 }),
            Dec {
                coeff: 5 * 10u128.pow(33),
                exp: -34
            }
        );
        assert_eq!(
            normalize(Dec { coeff: 0, exp: 0 }),
            Dec {
                coeff: 0,
                exp: MIN_EXP
            }
        );
    }

    #[test]
    fn a_continuation_that_overflows_must_be_exact() {
        let x = Dec {
            coeff: MAX_COEFF,
            exp: 0,
        };
        assert_eq!(add_continuation(x, 2), None);
        assert_eq!(
            add_continuation(x, 1),
            Some(Dec {
                coeff: 10u128.pow(33),
                exp: 1
            })
        );
    }

    #[test]
    fn multiply_truncates_toward_zero() {
        let third = Dec {
            coeff: 3_333_333_333_333_333_333_333_333_333_333_333,
            exp: -34,
        };
        assert_eq!(
            multiply(third, Dec { coeff: 3, exp: 0 }, Round::Down),
            Dec {
                coeff: 9_999_999_999_999_999_999_999_999_999_999_999,
                exp: -34
            }
        );
    }
}

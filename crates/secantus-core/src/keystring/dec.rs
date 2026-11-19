//! The slice of mongod's `Decimal128` arithmetic that KeyString uses, over
//! plain decimal digit vectors.
//!
//! KeyString never needs general decimal arithmetic. It needs: the decimal's
//! parts, `toDouble(kRoundTowardZero)` with the inexact flag, double to decimal
//! at 15 or 34 significant digits under a directed rounding mode, an exact
//! add / subtract, a truncating multiply, `normalize()` (pad the coefficient to
//! 34 digits), and an exact `quantize`. Each is ported here by VALUE; the
//! exponent of a result is mongod's wherever KeyString reads it (the decimal
//! exponent is half of what a TypeBits stores, so it is part of the format).

use bson::Decimal128;
use std::cmp::Ordering;

/// `Decimal128::kExponentBias`.
pub(super) const BIAS: i32 = 6176;
/// `Decimal128::kMaxBiasedExponent`.
pub(super) const MAX_BEXP: u32 = 12287;
/// Significant digits of a decimal128 coefficient.
pub(super) const COEF_DIGITS: usize = 34;

/// `Decimal128::RoundingMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    TowardZero,
    TowardPositive,
    TowardNegative,
    /// IEEE round-to-nearest, ties to even (the default of every arithmetic
    /// operation; KeyString only needs it for sums that must stay exact).
    NearestEven,
}

/// `Decimal128::RoundingPrecision` (the two KeyString uses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prec {
    Digits15,
    Digits34,
}

/// A decimal128 as mongod sees it before arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum D {
    Nan,
    Inf { neg: bool },
    Fin { neg: bool, coef: u128, bexp: u32 },
}

const TEN34: u128 = 10_000_000_000_000_000_000_000_000_000_000_000;

impl D {
    /// Read the BID words of a BSON decimal128.
    pub(super) fn from_words(hi: u64, lo: u64) -> D {
        let neg = hi >> 63 == 1;
        match (hi >> 58) & 0x1f {
            0x1f => return D::Nan,
            0x1e => return D::Inf { neg },
            _ => {}
        }
        if (hi >> 61) & 3 == 3 {
            // The large-coefficient form: the coefficient would be at least
            // 2**113, so it is non-canonical and reads as zero.
            return D::Fin {
                neg,
                coef: 0,
                bexp: ((hi >> 47) & 0x3fff) as u32,
            };
        }
        let bexp = ((hi >> 49) & 0x3fff) as u32;
        let coef = (((hi & ((1u64 << 49) - 1)) as u128) << 64) | lo as u128;
        D::Fin {
            neg,
            coef: if coef >= TEN34 { 0 } else { coef },
            bexp,
        }
    }

    pub(super) fn from_bson(d: &Decimal128) -> D {
        let b = d.bytes();
        let lo = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let hi = u64::from_le_bytes(b[8..16].try_into().unwrap());
        D::from_words(hi, lo)
    }

    /// `(high64, low64)` of the BID encoding (`Decimal128::getValue()`).
    pub(super) fn words(&self) -> (u64, u64) {
        match *self {
            D::Nan => (0x7c00_0000_0000_0000, 0),
            D::Inf { neg } => ((0x7800_0000_0000_0000u64) | ((neg as u64) << 63), 0),
            D::Fin { neg, coef, bexp } => (
                ((neg as u64) << 63) | ((bexp as u64) << 49) | (coef >> 64) as u64,
                coef as u64,
            ),
        }
    }

    pub(super) fn to_bson(self) -> Decimal128 {
        let (hi, lo) = self.words();
        let mut b = [0u8; 16];
        b[0..8].copy_from_slice(&lo.to_le_bytes());
        b[8..16].copy_from_slice(&hi.to_le_bytes());
        Decimal128::from_bytes(b)
    }

    /// `Decimal128(int64)`.
    pub(super) fn from_i64(v: i64) -> D {
        D::Fin {
            neg: v < 0,
            coef: v.unsigned_abs() as u128,
            bexp: BIAS as u32,
        }
    }

    pub(super) fn to_big(self) -> Option<Big> {
        match self {
            D::Fin { neg, coef, bexp } => Some(Big::from_u128(neg, coef, bexp as i32 - BIAS)),
            _ => None,
        }
    }
}

/// A finite decimal of any width: sign, digits (most significant first, no
/// leading zeros, empty for zero) and exponent. Value is `digits * 10^exp`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Big {
    pub neg: bool,
    pub digits: Vec<u8>,
    pub exp: i32,
}

fn digits_of(mut n: u128) -> Vec<u8> {
    let mut v = Vec::new();
    while n > 0 {
        v.push((n % 10) as u8);
        n /= 10;
    }
    v.reverse();
    v
}

fn trim_leading(v: &mut Vec<u8>) {
    let z = v.iter().take_while(|d| **d == 0).count();
    v.drain(..z);
}

impl Big {
    pub(super) fn from_u128(neg: bool, n: u128, exp: i32) -> Big {
        Big {
            neg,
            digits: digits_of(n),
            exp,
        }
    }

    pub(super) fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// The value as a `u128` when it is a non-negative integer that fits
    /// (ignoring the sign).
    pub(super) fn magnitude_u128(&self) -> Option<u128> {
        if self.exp < 0 {
            let cut = (-self.exp) as usize;
            if cut > self.digits.len() {
                return if self.digits.is_empty() {
                    Some(0)
                } else {
                    None
                };
            }
            if self.digits[self.digits.len() - cut..]
                .iter()
                .any(|d| *d != 0)
            {
                return None;
            }
            return Self::fold(&self.digits[..self.digits.len() - cut]);
        }
        let mut n = Self::fold(&self.digits)?;
        for _ in 0..self.exp {
            n = n.checked_mul(10)?;
        }
        Some(n)
    }

    fn fold(d: &[u8]) -> Option<u128> {
        let mut n: u128 = 0;
        for x in d {
            n = n.checked_mul(10)?.checked_add(*x as u128)?;
        }
        Some(n)
    }

    /// The coefficient as a `u128` (digits only, exponent ignored).
    pub(super) fn coef_u128(&self) -> Option<u128> {
        Self::fold(&self.digits)
    }

    pub(super) fn negated(&self) -> Big {
        Big {
            neg: !self.neg,
            digits: self.digits.clone(),
            exp: self.exp,
        }
    }

    fn top(&self) -> i32 {
        self.exp + self.digits.len() as i32
    }

    /// Compare magnitudes by value.
    pub(super) fn cmp_mag(&self, o: &Big) -> Ordering {
        match (self.is_zero(), o.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match self.top().cmp(&o.top()) {
            Ordering::Equal => {}
            other => return other,
        }
        let n = self.digits.len().max(o.digits.len());
        for i in 0..n {
            let a = self.digits.get(i).copied().unwrap_or(0);
            let b = o.digits.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        Ordering::Equal
    }

    /// Signed comparison by value (`-0 == 0`).
    pub(super) fn cmp_val(&self, o: &Big) -> Ordering {
        let (az, bz) = (self.is_zero(), o.is_zero());
        if az && bz {
            return Ordering::Equal;
        }
        let an = self.neg && !az;
        let bn = o.neg && !bz;
        match (an, bn) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_mag(o),
            (true, true) => o.cmp_mag(self),
        }
    }

    /// Exact sum; the exponent is the smaller of the two (IEEE preferred
    /// exponent), never rounded.
    pub(super) fn add(&self, o: &Big) -> Big {
        let emin = self.exp.min(o.exp);
        let widen = |b: &Big| {
            let mut v = b.digits.clone();
            if !v.is_empty() {
                v.extend(std::iter::repeat_n(0u8, (b.exp - emin) as usize));
            }
            v
        };
        let (a, b) = (widen(self), widen(o));
        if self.neg == o.neg {
            let mut s = add_mag(&a, &b);
            trim_leading(&mut s);
            return Big {
                neg: self.neg && !s.is_empty(),
                digits: s,
                exp: emin,
            };
        }
        match cmp_vec(&a, &b) {
            Ordering::Equal => Big {
                neg: false,
                digits: Vec::new(),
                exp: emin,
            },
            Ordering::Greater => {
                let mut d = sub_mag(&a, &b);
                trim_leading(&mut d);
                Big {
                    neg: self.neg,
                    digits: d,
                    exp: emin,
                }
            }
            Ordering::Less => {
                let mut d = sub_mag(&b, &a);
                trim_leading(&mut d);
                Big {
                    neg: o.neg,
                    digits: d,
                    exp: emin,
                }
            }
        }
    }

    pub(super) fn sub(&self, o: &Big) -> Big {
        self.add(&o.negated())
    }

    /// Exact product.
    pub(super) fn mul(&self, o: &Big) -> Big {
        let (a, b) = (&self.digits, &o.digits);
        if a.is_empty() || b.is_empty() {
            return Big {
                neg: self.neg != o.neg,
                digits: Vec::new(),
                exp: self.exp + o.exp,
            };
        }
        let mut acc = vec![0u32; a.len() + b.len()];
        for (i, x) in a.iter().enumerate().rev() {
            for (j, y) in b.iter().enumerate().rev() {
                acc[i + j + 1] += *x as u32 * *y as u32;
            }
        }
        for k in (1..acc.len()).rev() {
            acc[k - 1] += acc[k] / 10;
            acc[k] %= 10;
        }
        let mut digits: Vec<u8> = acc.into_iter().map(|d| d as u8).collect();
        trim_leading(&mut digits);
        Big {
            neg: self.neg != o.neg,
            digits,
            exp: self.exp + o.exp,
        }
    }

    /// Round to at most `n` significant digits. The flag is "inexact".
    pub(super) fn round(&self, n: usize, mode: Mode) -> (Big, bool) {
        if self.digits.len() <= n {
            return (self.clone(), false);
        }
        let drop = self.digits.len() - n;
        let (keep, rest) = self.digits.split_at(n);
        let inexact = rest.iter().any(|d| *d != 0);
        let up = inexact
            && match mode {
                Mode::TowardZero => false,
                Mode::TowardPositive => !self.neg,
                Mode::TowardNegative => self.neg,
                Mode::NearestEven => match rest[0].cmp(&5) {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => {
                        rest[1..].iter().any(|d| *d != 0) || keep.last().is_some_and(|d| d % 2 == 1)
                    }
                },
            };
        let mut out = keep.to_vec();
        let mut exp = self.exp + drop as i32;
        if up {
            let mut i = out.len();
            loop {
                if i == 0 {
                    out.insert(0, 1);
                    out.pop();
                    exp += 1;
                    break;
                }
                i -= 1;
                if out[i] == 9 {
                    out[i] = 0;
                } else {
                    out[i] += 1;
                    break;
                }
            }
        }
        (
            Big {
                neg: self.neg,
                digits: out,
                exp,
            },
            inexact,
        )
    }

    /// `Decimal128::normalize()` on a non-zero value: the coefficient padded
    /// with zeros up to 34 digits, as far as the minimum exponent allows.
    pub(super) fn normalized34(&self) -> Big {
        let mut b = self.clone();
        while b.digits.len() < COEF_DIGITS && b.exp > -BIAS {
            b.digits.push(0);
            b.exp -= 1;
        }
        b
    }

    /// Pad to exactly `n` digits (a value already that wide is unchanged).
    pub(super) fn padded_to(&self, n: usize) -> Big {
        let mut b = self.clone();
        while b.digits.len() < n && b.exp > -BIAS {
            b.digits.push(0);
            b.exp -= 1;
        }
        b
    }

    /// Truncate toward zero to an integer with exponent 0 (what
    /// `quantize(0E0, kRoundTowardZero)` does for a value this size). The flag
    /// is "inexact" (a non-zero digit was dropped).
    pub(super) fn truncate_to_integer(&self) -> (Big, bool) {
        if self.exp >= 0 {
            let mut d = self.digits.clone();
            if !d.is_empty() {
                d.extend(std::iter::repeat_n(0u8, self.exp as usize));
            }
            return (
                Big {
                    neg: self.neg,
                    digits: d,
                    exp: 0,
                },
                false,
            );
        }
        let cut = (-self.exp) as usize;
        if cut >= self.digits.len() {
            return (
                Big {
                    neg: self.neg,
                    digits: Vec::new(),
                    exp: 0,
                },
                !self.digits.is_empty(),
            );
        }
        let keep = self.digits.len() - cut;
        let inexact = self.digits[keep..].iter().any(|d| *d != 0);
        (
            Big {
                neg: self.neg,
                digits: self.digits[..keep].to_vec(),
                exp: 0,
            },
            inexact,
        )
    }

    /// Back to a decimal128 value, when it fits.
    pub(super) fn to_d(&self) -> Option<D> {
        if self.digits.len() > COEF_DIGITS {
            return None;
        }
        let bexp = self.exp + BIAS;
        if bexp < 0 || bexp as u32 > MAX_BEXP {
            return None;
        }
        Some(D::Fin {
            neg: self.neg,
            coef: self.coef_u128()?,
            bexp: bexp as u32,
        })
    }
}

fn cmp_vec(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().max(b.len());
    let pa = n - a.len();
    let pb = n - b.len();
    for i in 0..n {
        let x = if i < pa { 0 } else { a[i - pa] };
        let y = if i < pb { 0 } else { b[i - pb] };
        match x.cmp(&y) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

fn add_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let n = a.len().max(b.len());
    let mut out = vec![0u8; n + 1];
    let mut carry = 0u8;
    for i in 0..n {
        let x = if i < a.len() { a[a.len() - 1 - i] } else { 0 };
        let y = if i < b.len() { b[b.len() - 1 - i] } else { 0 };
        let s = x + y + carry;
        out[n - i] = s % 10;
        carry = s / 10;
    }
    out[0] = carry;
    out
}

/// `a - b` for `a >= b`.
fn sub_mag(a: &[u8], b: &[u8]) -> Vec<u8> {
    let n = a.len().max(b.len());
    let mut out = vec![0u8; n];
    let mut borrow = 0i8;
    for i in 0..n {
        let x = if i < a.len() {
            a[a.len() - 1 - i] as i8
        } else {
            0
        };
        let y = if i < b.len() {
            b[b.len() - 1 - i] as i8
        } else {
            0
        };
        let mut d = x - y - borrow;
        if d < 0 {
            d += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out[n - 1 - i] = d as u8;
    }
    out
}

/// The exact decimal expansion of a finite, non-zero double's magnitude, with
/// every trailing zero stripped (so an integer like 1e22 carries a positive
/// exponent).
pub(super) fn f64_exact(x: f64) -> Big {
    debug_assert!(x.is_finite() && x != 0.0);
    // 800 significant digits write any f64 exactly (the maximum is 767).
    let s = format!("{:.*e}", 799, x.abs());
    let (mant, e) = s.split_once('e').expect("exponent form");
    let e: i32 = e.parse().expect("exponent");
    let mut digits: Vec<u8> = mant
        .bytes()
        .filter(|b| *b != b'.')
        .map(|b| b - b'0')
        .collect();
    let mut exp = e - 799;
    while digits.last() == Some(&0) {
        digits.pop();
        exp += 1;
    }
    Big {
        neg: x < 0.0,
        digits,
        exp,
    }
}

/// `Decimal128(double, precision, mode)`.
///
/// 15 digits is exactly 15 digits of the true binary value, padded; 34 digits
/// is the exact value with trailing zeros kept only up to exponent 0 (an
/// integral double keeps its zeros in the coefficient), rounded to 34 digits
/// when longer. Both under the directed rounding `mode`.
pub(super) fn from_double(x: f64, prec: Prec, mode: Mode) -> Option<D> {
    if x.is_nan() {
        return Some(D::Nan);
    }
    if x.is_infinite() {
        return Some(D::Inf { neg: x < 0.0 });
    }
    if x == 0.0 {
        return Some(D::Fin {
            neg: x.is_sign_negative(),
            coef: 0,
            bexp: BIAS as u32,
        });
    }
    let mut b = f64_exact(x);
    match prec {
        Prec::Digits34 => {
            if b.exp > 0 {
                b.digits.extend(std::iter::repeat_n(0u8, b.exp as usize));
                b.exp = 0;
            }
            b = b.round(COEF_DIGITS, mode).0;
        }
        Prec::Digits15 => {
            b = b.round(15, mode).0.padded_to(15);
        }
    }
    b.to_d()
}

/// The next double below `x` toward zero, for a positive finite `x`.
fn next_toward_zero(x: f64) -> f64 {
    x.next_down()
}

/// `Decimal128::toDouble(kRoundTowardZero)`: the double, whether the decimal
/// was not exactly that double, and whether it overflowed the finite range
/// (then the largest finite double comes back, as the Intel library returns).
pub(super) fn to_double_toward_zero(b: &Big) -> (f64, bool, bool) {
    let sign = if b.neg { -1.0 } else { 1.0 };
    if b.is_zero() {
        return (sign * 0.0, false, false);
    }
    let text: String = b.digits.iter().map(|d| (b'0' + d) as char).collect();
    let cand: f64 = format!("{text}e{}", b.exp).parse().expect("decimal text");
    if cand.is_infinite() {
        return (sign * f64::MAX, true, true);
    }
    if cand == 0.0 {
        return (sign * 0.0, true, false);
    }
    let exact = f64_exact(cand);
    match exact.cmp_mag(b) {
        Ordering::Equal => (sign * cand, false, false),
        Ordering::Less => (sign * cand, true, false),
        Ordering::Greater => (sign * next_toward_zero(cand), true, false),
    }
}

/// `2**256` and `2**-256` as decimals truncated toward zero to 34 digits
/// (`kTinyDoubleExponentUpshiftFactorAsDecimal` and its downshift twin).
pub(super) fn tiny_upshift() -> Big {
    let x = f64::from_bits(((256 + 1023) as u64) << 52);
    from_double(x, Prec::Digits34, Mode::TowardZero)
        .and_then(|d| d.to_big())
        .expect("2**256")
}

pub(super) fn tiny_downshift() -> Big {
    let x = f64::from_bits(((-256 + 1023) as u64) << 52);
    from_double(x, Prec::Digits34, Mode::TowardZero)
        .and_then(|d| d.to_big())
        .expect("2**-256")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(neg: bool, n: u128, exp: i32) -> Big {
        Big::from_u128(neg, n, exp)
    }

    #[test]
    fn exact_double_conversions() {
        let d = from_double(0.5, Prec::Digits34, Mode::TowardZero).unwrap();
        assert_eq!(d.to_big().unwrap(), big(false, 5, -1));
        let d = from_double(1e10, Prec::Digits34, Mode::TowardZero).unwrap();
        assert_eq!(d.to_big().unwrap(), big(false, 10_000_000_000, 0));
        // 15 digits, padded.
        let d = from_double(0.1, Prec::Digits15, Mode::NearestEven).unwrap();
        assert_eq!(d.to_big().unwrap(), big(false, 100_000_000_000_000, -15));
        // 0.1 as a double is a hair under 0.1: rounding UP at 15 digits gives
        // 0.1, rounding toward zero gives 0.099999999999999.
        let bin = 0.1f64.next_down();
        let up = from_double(bin, Prec::Digits15, Mode::TowardPositive).unwrap();
        assert_eq!(up.to_big().unwrap(), big(false, 100_000_000_000_000, -15));
    }

    #[test]
    fn decimal_to_double_toward_zero() {
        assert_eq!(
            to_double_toward_zero(&big(false, 1, 0)),
            (1.0, false, false)
        );
        assert_eq!(
            to_double_toward_zero(&big(false, 15, -1)),
            (1.5, false, false)
        );
        let (d, inexact, _) = to_double_toward_zero(&big(false, 1, -1));
        assert!(inexact);
        assert_eq!(d, 0.1f64.next_down());
        let (d, inexact, overflow) = to_double_toward_zero(&big(false, 1, 400));
        assert!(inexact && overflow);
        assert_eq!(d, f64::MAX);
        let (d, inexact, _) = to_double_toward_zero(&big(true, 1, -400));
        assert!(inexact);
        assert_eq!(d, 0.0);
    }

    #[test]
    fn add_sub_mul_round() {
        let a = big(false, 12345, -2); // 123.45
        let b = big(false, 5, -1); // 0.5
        assert_eq!(a.add(&b), big(false, 12395, -2));
        assert_eq!(a.sub(&a), big(false, 0, -2));
        assert_eq!(a.sub(&big(false, 20000, -2)), big(true, 7655, -2));
        assert_eq!(b.mul(&b), big(false, 25, -2));
        let (r, inexact) = big(false, 123456, 0).round(3, Mode::NearestEven);
        assert!(inexact);
        assert_eq!(r, big(false, 123, 3));
        let (r, _) = big(false, 999999, 0).round(3, Mode::TowardPositive);
        assert_eq!(r, big(false, 100, 4));
        let (r, _) = big(true, 123456, 0).round(3, Mode::TowardPositive);
        assert_eq!(r, big(true, 123, 3));
    }

    #[test]
    fn words_round_trip() {
        for d in [
            D::Fin {
                neg: false,
                coef: 1,
                bexp: 6176,
            },
            D::Fin {
                neg: true,
                coef: TEN34 - 1,
                bexp: 12287,
            },
            D::Fin {
                neg: false,
                coef: 0,
                bexp: 0,
            },
        ] {
            let (hi, lo) = d.words();
            assert_eq!(D::from_words(hi, lo), d);
            assert_eq!(D::from_bson(&d.to_bson()), d);
        }
        assert_eq!(D::from_bson(&"NaN".parse::<Decimal128>().unwrap()), D::Nan);
    }
}

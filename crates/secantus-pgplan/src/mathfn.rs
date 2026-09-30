//! The mathematical functions beyond arithmetic: trigonometry in radians and
//! in degrees, the hyperbolic functions, `cbrt`, `pi` / `degrees` /
//! `radians`, `gcd` / `lcm`, `factorial`, `width_bucket` and `setseed`.
//!
//! The degree-based functions follow PostgreSQL's `float.c` exactly: each
//! reduces its argument to the first quadrant and scales by constants
//! computed from the radian functions, so `sind(30)` is exactly `0.5` and
//! `tand(45)` exactly `1` -- the whole reason they exist. Measured against
//! PostgreSQL 14.

use bson::Bson;
use num_bigint::BigInt;
use num_traits::{One, Signed, Zero};

use crate::{Error, Result};

const RADIANS_PER_DEGREE: f64 = std::f64::consts::PI / 180.0;

fn f(args: &[Bson], i: usize, name: &str) -> Result<f64> {
    match args.get(i) {
        Some(Bson::Double(d)) => Ok(*d),
        Some(Bson::Int32(v)) => Ok(f64::from(*v)),
        Some(Bson::Int64(v)) => Ok(*v as f64),
        Some(other) => crate::numeric_text(other)
            .and_then(|t| crate::numeric::numeric_text_to_f64(&t))
            .ok_or_else(|| Error::UndefinedFunction(format!("function {name} does not exist"))),
        None => Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        ))),
    }
}

fn out_of_range() -> Error {
    Error::Sqlstate("22003", "input is out of range".into())
}

fn sin_30() -> f64 {
    (30.0 * RADIANS_PER_DEGREE).sin()
}
fn one_minus_cos_60() -> f64 {
    1.0 - (60.0 * RADIANS_PER_DEGREE).cos()
}
fn asin_0_5() -> f64 {
    0.5f64.asin()
}
fn acos_0_5() -> f64 {
    0.5f64.acos()
}
fn atan_1_0() -> f64 {
    1.0f64.atan()
}

fn sind_0_to_30(x: f64) -> f64 {
    (x * RADIANS_PER_DEGREE).sin() / sin_30() / 2.0
}
fn cosd_0_to_60(x: f64) -> f64 {
    1.0 - ((1.0 - (x * RADIANS_PER_DEGREE).cos()) / one_minus_cos_60()) / 2.0
}
fn sind_q1(x: f64) -> f64 {
    if x <= 30.0 {
        sind_0_to_30(x)
    } else {
        cosd_0_to_60(90.0 - x)
    }
}
fn cosd_q1(x: f64) -> f64 {
    if x <= 60.0 {
        cosd_0_to_60(x)
    } else {
        sind_0_to_30(90.0 - x)
    }
}
fn asind_q1(x: f64) -> f64 {
    if x <= 0.5 {
        (x.asin() / asin_0_5()) * 30.0
    } else {
        90.0 - (x.acos() / acos_0_5()) * 60.0
    }
}
fn acosd_q1(x: f64) -> f64 {
    if x <= 0.5 {
        90.0 - (x.asin() / asin_0_5()) * 30.0
    } else {
        (x.acos() / acos_0_5()) * 60.0
    }
}

/// The degree trigonometry, per `dsind` / `dcosd` / `dtand` / `dcotd`.
fn degree_trig(name: &str, x: f64) -> Result<f64> {
    if x.is_nan() {
        return Ok(f64::NAN);
    }
    if x.is_infinite() {
        return Err(out_of_range());
    }
    let mut a = x % 360.0;
    let mut sign = 1.0;
    Ok(match name {
        "sind" => {
            if a < 0.0 {
                a = -a;
                sign = -sign;
            }
            if a > 180.0 {
                a = 360.0 - a;
                sign = -sign;
            }
            if a > 90.0 {
                a = 180.0 - a;
            }
            sign * sind_q1(a)
        }
        "cosd" => {
            if a < 0.0 {
                a = -a;
            }
            if a > 180.0 {
                a = 360.0 - a;
            }
            if a > 90.0 {
                a = 180.0 - a;
                sign = -sign;
            }
            sign * cosd_q1(a)
        }
        _ => {
            // tand / cotd.
            if a < 0.0 {
                a = -a;
                sign = -sign;
            }
            if a > 180.0 {
                a = 360.0 - a;
                sign = -sign;
            }
            if a > 90.0 {
                a = 180.0 - a;
                sign = -sign;
            }
            let tan_45 = sind_q1(45.0) / cosd_q1(45.0);
            let v = if name == "tand" {
                sind_q1(a) / cosd_q1(a) / tan_45
            } else {
                cosd_q1(a) / sind_q1(a) / tan_45
            };
            let r = sign * v;
            // No minus zero.
            if r == 0.0 {
                0.0
            } else {
                r
            }
        }
    })
}

fn int_of(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        _ => None,
    }
}

/// A numeric as `(unscaled integer, scale)`.
fn scaled(v: &Bson) -> Option<(BigInt, u32)> {
    let text = crate::numeric_text(v)?;
    let (neg, body) = match text.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, text.clone()),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (body, String::new()),
    };
    let digits: BigInt = format!("{int}{frac}").parse().ok()?;
    Some((if neg { -digits } else { digits }, frac.len() as u32))
}

fn from_scaled(n: &BigInt, scale: u32) -> Result<Bson> {
    let neg = n.is_negative();
    let mut digits = n.abs().to_string();
    let scale = scale as usize;
    if scale > 0 {
        while digits.len() <= scale {
            digits.insert(0, '0');
        }
        digits.insert(digits.len() - scale, '.');
    }
    crate::numeric::parse_numeric(&format!("{}{digits}", if neg { "-" } else { "" }))
}

fn big_gcd(a: &BigInt, b: &BigInt) -> BigInt {
    let (mut a, mut b) = (a.abs(), b.abs());
    while !b.is_zero() {
        let t = &a % &b;
        a = b;
        b = t;
    }
    a
}

fn gcd_lcm(name: &str, args: &[Bson]) -> Result<Bson> {
    let [a, b] = args else {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    };
    // Integers stay integers of the wider operand's width.
    if let (Some(x), Some(y)) = (int_of(a), int_of(b)) {
        let wide = matches!(a, Bson::Int64(_)) || matches!(b, Bson::Int64(_));
        let (bx, by) = (BigInt::from(x), BigInt::from(y));
        let g = big_gcd(&bx, &by);
        let r = if name == "gcd" {
            g
        } else if g.is_zero() {
            BigInt::zero()
        } else {
            (&bx / &g * &by).abs()
        };
        let too_big = || {
            Error::NumericOutOfRange(
                (if wide {
                    "bigint out of range"
                } else {
                    "integer out of range"
                })
                .into(),
            )
        };
        return Ok(if wide {
            Bson::Int64(i64::try_from(r).map_err(|_| too_big())?)
        } else {
            Bson::Int32(i32::try_from(r).map_err(|_| too_big())?)
        });
    }
    let (Some((x, sx)), Some((y, sy))) = (scaled(a), scaled(b)) else {
        return Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        )));
    };
    let scale = sx.max(sy);
    let x = x * BigInt::from(10).pow(scale - sx);
    let y = y * BigInt::from(10).pow(scale - sy);
    let g = big_gcd(&x, &y);
    let r = if name == "gcd" {
        g
    } else if g.is_zero() {
        BigInt::zero()
    } else {
        // lcm(a, b) = |a * b| / gcd, in the common scale.
        (&x / &g * &y).abs()
    };
    from_scaled(&r, scale)
}

fn factorial(args: &[Bson]) -> Result<Bson> {
    let Some(n) = args.first().and_then(int_of) else {
        return Err(Error::UndefinedFunction(
            "function factorial does not exist".into(),
        ));
    };
    if n < 0 {
        return Err(Error::Sqlstate(
            "22003",
            "factorial of a negative number is undefined".into(),
        ));
    }
    if n > 32177 {
        return Err(Error::Sqlstate(
            "22003",
            "value overflows numeric format".into(),
        ));
    }
    let mut acc = BigInt::one();
    for i in 2..=n {
        acc *= i;
    }
    crate::numeric::parse_numeric(&acc.to_string())
}

/// `width_bucket(operand, low, high, count)` and `width_bucket(operand,
/// thresholds)`.
fn width_bucket(args: &[Bson]) -> Result<Bson> {
    if let [op, Bson::Array(thresholds)] = args {
        // The number of thresholds at or below the operand (the thresholds
        // are sorted, as PostgreSQL requires).
        let x = f(std::slice::from_ref(op), 0, "width_bucket")?;
        let mut n = 0;
        for t in thresholds {
            if t == &Bson::Null {
                return Err(Error::Sqlstate(
                    "22004",
                    "thresholds array must not contain NULLs".into(),
                ));
            }
            if f(std::slice::from_ref(t), 0, "width_bucket")? <= x {
                n += 1;
            } else {
                break;
            }
        }
        return Ok(Bson::Int32(n));
    }
    let [_, _, _, count] = args else {
        return Err(Error::UndefinedFunction(
            "function width_bucket does not exist".into(),
        ));
    };
    let (x, lo, hi) = (
        f(args, 0, "width_bucket")?,
        f(args, 1, "width_bucket")?,
        f(args, 2, "width_bucket")?,
    );
    let count = int_of(count)
        .ok_or_else(|| Error::UndefinedFunction("function width_bucket does not exist".into()))?;
    if count <= 0 {
        return Err(Error::Sqlstate(
            "2201G",
            "count must be greater than zero".into(),
        ));
    }
    if lo == hi {
        return Err(Error::Sqlstate(
            "2201G",
            "lower bound cannot equal upper bound".into(),
        ));
    }
    if x.is_nan() || lo.is_nan() || hi.is_nan() {
        return Err(Error::Sqlstate(
            "2201G",
            "operand, lower bound, and upper bound cannot be NaN".into(),
        ));
    }
    let c = count as f64;
    let bucket = if lo < hi {
        if x < lo {
            0
        } else if x >= hi {
            count + 1
        } else {
            ((x - lo) / (hi - lo) * c).floor() as i64 + 1
        }
    } else if x > lo {
        0
    } else if x <= hi {
        count + 1
    } else {
        ((lo - x) / (lo - hi) * c).floor() as i64 + 1
    };
    Ok(Bson::Int32(i32::try_from(bucket).map_err(|_| {
        Error::NumericOutOfRange("integer out of range".into())
    })?))
}

pub const FUNCTIONS: &[&str] = &[
    "sin",
    "cos",
    "tan",
    "cot",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sind",
    "cosd",
    "tand",
    "cotd",
    "asind",
    "acosd",
    "atand",
    "atan2d",
    "sinh",
    "cosh",
    "tanh",
    "asinh",
    "acosh",
    "atanh",
    "cbrt",
    "pi",
    "degrees",
    "radians",
    "gcd",
    "lcm",
    "factorial",
    "width_bucket",
    "setseed",
];

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "width_bucket" => "int4",
        "factorial" => "numeric",
        "setseed" => "void",
        // gcd / lcm follow their operands; see `static_type`.
        "gcd" | "lcm" => return None,
        n if FUNCTIONS.contains(&n) => "float8",
        _ => return None,
    })
}

pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !FUNCTIONS.contains(&name) {
        return None;
    }
    if name != "pi" && args.contains(&Bson::Null) && name != "width_bucket" {
        return Some(Ok(Bson::Null));
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    let x = || f(args, 0, name);
    let v = match name {
        "pi" => std::f64::consts::PI,
        "sin" => x()?.sin(),
        "cos" => x()?.cos(),
        "tan" => x()?.tan(),
        "cot" => 1.0 / x()?.tan(),
        "asin" | "acos" => {
            let a = x()?;
            if !(-1.0..=1.0).contains(&a) {
                return Err(out_of_range());
            }
            if name == "asin" {
                a.asin()
            } else {
                a.acos()
            }
        }
        "atan" => x()?.atan(),
        "atan2" => f(args, 0, name)?.atan2(f(args, 1, name)?),
        "sind" | "cosd" | "tand" | "cotd" => degree_trig(name, x()?)?,
        "asind" | "acosd" => {
            let a = x()?;
            if a.is_nan() {
                return Ok(Bson::Double(f64::NAN));
            }
            if !(-1.0..=1.0).contains(&a) {
                return Err(out_of_range());
            }
            match (name, a >= 0.0) {
                ("asind", true) => asind_q1(a),
                ("asind", false) => -asind_q1(-a),
                (_, true) => acosd_q1(a),
                (_, false) => 90.0 + asind_q1(-a),
            }
        }
        "atand" => (x()?.atan() / atan_1_0()) * 45.0,
        "atan2d" => (f(args, 0, name)?.atan2(f(args, 1, name)?) / atan_1_0()) * 45.0,
        "sinh" => x()?.sinh(),
        "cosh" => x()?.cosh(),
        "tanh" => x()?.tanh(),
        "asinh" => x()?.asinh(),
        "acosh" => {
            let a = x()?;
            if a < 1.0 {
                return Err(out_of_range());
            }
            a.acosh()
        }
        "atanh" => {
            let a = x()?;
            if !(-1.0..=1.0).contains(&a) {
                return Err(out_of_range());
            }
            a.atanh()
        }
        "cbrt" => x()?.cbrt(),
        "degrees" => x()? / RADIANS_PER_DEGREE,
        "radians" => x()? * RADIANS_PER_DEGREE,
        "gcd" | "lcm" => return gcd_lcm(name, args),
        "factorial" => return factorial(args),
        "width_bucket" => {
            if args.contains(&Bson::Null) {
                return Ok(Bson::Null);
            }
            return width_bucket(args);
        }
        "setseed" => {
            let s = x()?;
            if !(-1.0..=1.0).contains(&s) {
                return Err(Error::Sqlstate(
                    "22003",
                    format!("setseed parameter {s} is out of allowed range [-1,1]"),
                ));
            }
            return Ok(Bson::String(String::new()));
        }
        _ => {
            return Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            )))
        }
    };
    Ok(Bson::Double(v))
}

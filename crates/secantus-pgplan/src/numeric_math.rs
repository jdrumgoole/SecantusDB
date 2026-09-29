//! The `numeric` transcendental functions -- `sqrt`, `exp`, `ln`, `log`,
//! `power` -- and the scale functions `scale` / `min_scale` / `trim_scale`.
//!
//! What a client sees of these is the RESULT SCALE, and PostgreSQL chooses it
//! by rules that have nothing to do with the value's precision: at least 16
//! significant digits, never fewer places than an input's, estimated from a
//! float approximation of the result's weight. Those rules are transcribed
//! from PostgreSQL 14's `numeric.c` (`numeric_sqrt`, `numeric_exp`,
//! `numeric_ln`, `log_var`, `power_var`, `estimate_ln_dweight`), including
//! its base-10000 `weight` and C's truncating `(int)` casts.
//!
//! The VALUE is then computed in BigInt fixed point with guard digits and
//! rounded half away from zero at that scale. PostgreSQL computes with guard
//! digits too and rounds once, so both land on the correctly rounded result.
//! `sqrt` and an integer `power` are exact.

// PostgreSQL's own literals (`0.434294481903252`, `2.302585092994046`) are
// kept verbatim: they decide result scales, and `LOG10_E` is not the same
// double as `0.434294481903252`.
#![allow(clippy::approx_constant)]

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::numeric::{canonical_numeric_text, round_half_away, Dec};
use crate::{Error, Result};

const MIN_SIG_DIGITS: i64 = 16;
const MAX_DISPLAY_SCALE: i64 = 1000;
const MAX_RESULT_SCALE: f64 = 2000.0;
/// Extra decimal places carried through every fixed-point computation.
const GUARD: u32 = 16;

fn p10(k: u32) -> BigInt {
    BigInt::from(10u32).pow(k)
}

fn clamp_scale(r: i64) -> u32 {
    r.clamp(0, MAX_DISPLAY_SCALE) as u32
}

fn power_error(msg: &str) -> Error {
    Error::Sqlstate("2201F", msg.into())
}

fn log_error(msg: &str) -> Error {
    Error::Sqlstate("2201E", msg.into())
}

fn overflow() -> Error {
    Error::Sqlstate("22003", "value overflows numeric format".into())
}

/// `|d|` in PostgreSQL's base-10000 form: the weight of the leading group
/// and the groups, without leading or trailing zero groups. Zero is `(0, [])`.
fn nbase(d: &Dec) -> (i64, Vec<i64>) {
    let text = d.unscaled.abs().to_string();
    let s = d.scale as usize;
    let digits = if text.len() <= s {
        format!("{}{text}", "0".repeat(s - text.len() + 1))
    } else {
        text
    };
    let (int, frac) = digits.split_at(digits.len() - s);
    let int = format!("{}{int}", "0".repeat((4 - int.len() % 4) % 4));
    let frac = format!("{frac}{}", "0".repeat((4 - frac.len() % 4) % 4));
    let mut weight = (int.len() / 4) as i64 - 1;
    let all = format!("{int}{frac}");
    let mut groups: Vec<i64> = all
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or("0").parse().unwrap_or(0))
        .collect();
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        return (0, Vec::new());
    }
    (weight, groups)
}

fn cmp(a: &Dec, b: &Dec) -> std::cmp::Ordering {
    let s = a.scale.max(b.scale);
    (&a.unscaled * p10(s - a.scale)).cmp(&(&b.unscaled * p10(s - b.scale)))
}

fn dec(unscaled: i64, scale: u32) -> Dec {
    Dec {
        unscaled: BigInt::from(unscaled),
        scale,
    }
}

fn to_f64(d: &Dec) -> f64 {
    d.render().parse().unwrap_or(0.0)
}

/// `estimate_ln_dweight`: the approximate decimal weight of `ln(d)`.
fn estimate_ln_dweight(d: &Dec) -> i64 {
    if !d.unscaled.is_positive() {
        return 0;
    }
    if cmp(d, &dec(9, 1)).is_ge() && cmp(d, &dec(11, 1)).is_le() {
        let s = d.scale.max(1);
        let x = Dec {
            unscaled: &d.unscaled * p10(s - d.scale) - p10(s),
            scale: s,
        };
        let (w, g) = nbase(&x);
        return match g.first() {
            Some(&first) => w * 4 + (first as f64).log10() as i64,
            None => 0,
        };
    }
    let (w, g) = nbase(d);
    let Some(&first) = g.first() else { return 0 };
    let mut digits = first;
    let mut dweight = w * 4;
    if let Some(&second) = g.get(1) {
        digits = digits * 10000 + second;
        dweight -= 4;
    }
    let ln = (digits as f64).ln() + dweight as f64 * 2.302585092994046;
    ln.abs().log10() as i64
}

/// A fixed-point value `v / 10^s` rounded half away from zero to `r` places.
fn round_to(v: &BigInt, s: u32, r: u32) -> Dec {
    let unscaled = if s >= r {
        let q = round_half_away(&v.abs(), &p10(s - r));
        if v.is_negative() {
            -q
        } else {
            q
        }
    } else {
        v * p10(r - s)
    };
    Dec { unscaled, scale: r }
}

/// `ln(y)` for a fixed-point `y` (scale `s`) in `[1, 10]`: square roots
/// until it is within 1% of one, then the `atanh` series.
fn ln_unit(y: &BigInt, s: u32) -> BigInt {
    let one = p10(s);
    let near = &one + &one / 100;
    let mut y = y.clone();
    let mut k = 0u32;
    while y > near {
        y = (&y * &one).sqrt();
        k += 1;
    }
    let z = (&y - &one) * &one / (&y + &one);
    let z2 = &z * &z / &one;
    let mut term = z.clone();
    let mut sum = z;
    let mut i = 1u32;
    loop {
        term = &term * &z2 / &one;
        if term.is_zero() {
            break;
        }
        sum += &term / BigInt::from(2 * i + 1);
        i += 1;
    }
    sum * 2 * BigInt::from(2u32).pow(k)
}

/// `ln(d)` for a positive `d`, as a fixed-point value at scale `w`.
fn ln_fixed(d: &Dec, w: u32) -> BigInt {
    let wp = w + GUARD;
    let u = d.unscaled.abs();
    let nd = u.to_string().len() as u32;
    // d = m * 10^e with m in [1, 10).
    let e = i64::from(nd) - 1 - i64::from(d.scale);
    let m = if wp >= nd - 1 {
        &u * p10(wp - (nd - 1))
    } else {
        &u / p10(nd - 1 - wp)
    };
    let mut r = ln_unit(&m, wp);
    if e != 0 {
        r += ln_unit(&(p10(wp) * 10), wp) * BigInt::from(e);
    }
    round_to(&r, wp, w).unscaled
}

/// `exp(x)` for a fixed-point `x` (scale `s`), rounded to `rscale` places.
fn exp_fixed(x: &BigInt, s: u32, rscale: u32) -> Result<Dec> {
    let xf = round_to(x, s, 17.min(s))
        .render()
        .parse::<f64>()
        .unwrap_or(0.0);
    if xf > MAX_RESULT_SCALE * 3.0 * 2.302585092994046 / 0.999 {
        return Err(overflow());
    }
    let dweight = (xf * 0.434294481903252).floor().max(0.0) as u32 + 1;
    // Halve until |r| < 0.001, and square back afterwards.
    let k = if xf.abs() > 0.001 {
        (xf.abs() / 0.001).log2().ceil() as u32
    } else {
        0
    };
    let ws = rscale + GUARD + dweight + (k * 3).div_ceil(10) + 2;
    let one = p10(ws);
    let xw = if ws >= s {
        x * p10(ws - s)
    } else {
        x / p10(s - ws)
    };
    let r = xw / BigInt::from(2u32).pow(k);
    let mut sum = one.clone();
    let mut term = one.clone();
    let mut i = 1u32;
    loop {
        term = &term * &r / &one / BigInt::from(i);
        if term.is_zero() {
            break;
        }
        sum += &term;
        i += 1;
    }
    for _ in 0..k {
        sum = &sum * &sum / &one;
    }
    Ok(round_to(&sum, ws, rscale))
}

fn render(d: &Dec) -> Result<String> {
    canonical_numeric_text(&d.render())
}

fn sqrt(x: &Dec) -> Result<String> {
    if x.unscaled.is_negative() {
        return Err(power_error("cannot take square root of a negative number"));
    }
    let (w, _) = nbase(x);
    let sweight = (w + 1) * 4 / 2 - 1;
    let rscale = clamp_scale((MIN_SIG_DIGITS - sweight).max(i64::from(x.scale)));
    // floor(sqrt(x) * 10^(rscale+1)), then the last digit rounds.
    let t = (&x.unscaled * p10(2 * rscale + 2 - x.scale)).sqrt();
    let q = round_half_away(&t, &BigInt::from(10u32));
    render(&Dec {
        unscaled: q,
        scale: rscale,
    })
}

fn exp(x: &Dec) -> Result<String> {
    let val = (to_f64(x) * 0.434294481903252).clamp(-MAX_RESULT_SCALE, MAX_RESULT_SCALE);
    let rscale = clamp_scale((MIN_SIG_DIGITS - val as i64).max(i64::from(x.scale)));
    render(&exp_fixed(&x.unscaled, x.scale, rscale)?)
}

fn check_log_arg(x: &Dec) -> Result<()> {
    if x.unscaled.is_zero() {
        return Err(log_error("cannot take logarithm of zero"));
    }
    if x.unscaled.is_negative() {
        return Err(log_error("cannot take logarithm of a negative number"));
    }
    Ok(())
}

fn ln(x: &Dec) -> Result<String> {
    check_log_arg(x)?;
    let rscale = clamp_scale((MIN_SIG_DIGITS - estimate_ln_dweight(x)).max(i64::from(x.scale)));
    render(&round_to(
        &ln_fixed(x, rscale + GUARD),
        rscale + GUARD,
        rscale,
    ))
}

/// `log_var`: `log(base, num)`, with its own scale rule.
fn log(base: &Dec, num: &Dec) -> Result<String> {
    check_log_arg(base)?;
    check_log_arg(num)?;
    let result_dweight = estimate_ln_dweight(num) - estimate_ln_dweight(base);
    let rscale = clamp_scale(
        (MIN_SIG_DIGITS - result_dweight)
            .max(i64::from(base.scale))
            .max(i64::from(num.scale)),
    );
    let w = rscale + GUARD + result_dweight.unsigned_abs() as u32;
    let lb = ln_fixed(base, w);
    if lb.is_zero() {
        return Err(Error::DivisionByZero);
    }
    let ln_num = ln_fixed(num, w);
    // (ln_num / lb) at scale w, then rounded.
    let neg = ln_num.is_negative() != lb.is_negative();
    let q = round_half_away(&(ln_num.abs() * p10(rscale)), &lb.abs());
    render(&Dec {
        unscaled: if neg { -q } else { q },
        scale: rscale,
    })
}

fn is_integral(d: &Dec) -> bool {
    (&d.unscaled % p10(d.scale)).is_zero()
}

fn power(base: &Dec, exp: &Dec) -> Result<String> {
    if base.unscaled.is_zero() && exp.unscaled.is_negative() {
        return Err(power_error("zero raised to a negative power is undefined"));
    }
    if is_integral(exp) {
        let n = (&exp.unscaled / p10(exp.scale)).to_i64();
        if let Some(n) = n.filter(|n| i32::try_from(*n).is_ok()) {
            let rscale = clamp_scale(MIN_SIG_DIGITS.max(i64::from(base.scale)));
            return render(&power_int(base, n, rscale)?);
        }
    }
    if base.unscaled.is_zero() {
        return render(&Dec {
            unscaled: BigInt::zero(),
            scale: MIN_SIG_DIGITS as u32,
        });
    }
    let mut abs_base = base.clone();
    let mut negative = false;
    if base.unscaled.is_negative() {
        if !is_integral(exp) {
            return Err(power_error(
                "a negative number raised to a non-integer power yields a complex result",
            ));
        }
        negative = (&exp.unscaled / p10(exp.scale)) % 2 != BigInt::zero();
        abs_base.unscaled = -abs_base.unscaled;
    }
    let base = &abs_base;
    let ln_dweight = estimate_ln_dweight(base);
    let local = (8 - ln_dweight).max(0) as u32;
    // The low-precision `exp * ln(base)`, as power_var rounds it.
    let ln_low = round_to(&ln_fixed(base, local), local, local);
    let prod = Dec {
        unscaled: &ln_low.unscaled * &exp.unscaled,
        scale: local + exp.scale,
    };
    let prod = round_to(&prod.unscaled, prod.scale, local);
    let val = to_f64(&prod);
    if val.abs() > MAX_RESULT_SCALE * 3.01 {
        if val > 0.0 {
            return Err(overflow());
        }
        return render(&Dec {
            unscaled: BigInt::zero(),
            scale: MAX_DISPLAY_SCALE as u32,
        });
    }
    let val = val * 0.434294481903252;
    let rscale = clamp_scale(
        (MIN_SIG_DIGITS - val as i64)
            .max(i64::from(base.scale))
            .max(i64::from(exp.scale)),
    );
    // exp(exp * ln(base)) at enough places that the rounding is right.
    let w = rscale + GUARD + (val.max(0.0) as u32) + exp_int_digits(exp);
    let ln_base = ln_fixed(base, w);
    let t = &ln_base * &exp.unscaled;
    let mut out = exp_fixed(&t, w + exp.scale, rscale)?;
    if negative && !out.unscaled.is_zero() {
        out.unscaled = -out.unscaled;
    }
    render(&out)
}

fn exp_int_digits(exp: &Dec) -> u32 {
    (&exp.unscaled / p10(exp.scale)).abs().to_string().len() as u32
}

/// `power_var_int`: an integer exponent, computed exactly where the exact
/// value is of a sane size.
fn power_int(base: &Dec, n: i64, rscale: u32) -> Result<Dec> {
    if n == 0 {
        return Ok(Dec {
            unscaled: p10(rscale),
            scale: rscale,
        });
    }
    if base.unscaled.is_zero() {
        return Ok(Dec {
            unscaled: BigInt::zero(),
            scale: rscale,
        });
    }
    // The crude weight estimate power_var_int makes before multiplying.
    let (w, g) = nbase(base);
    let mut f = g[0] as f64;
    let mut p = w * 4;
    for (i, d) in g.iter().enumerate().skip(1) {
        if i * 4 >= 16 {
            break;
        }
        f = f * 10000.0 + *d as f64;
        p -= 4;
    }
    let f = n as f64 * (f.log10() + p as f64);
    if f > 3.0 * 32767.0 * 4.0 {
        return Err(overflow());
    }
    if f + 1.0 < -(rscale as f64) {
        return Ok(Dec {
            unscaled: BigInt::zero(),
            scale: rscale,
        });
    }
    let digits = base.unscaled.abs().to_string().len() as u64;
    if n.unsigned_abs().saturating_mul(digits) > 400_000 {
        // Too large to build exactly: go through exp(n * ln|base|).
        let neg = base.unscaled.is_negative() && n % 2 != 0;
        let mut abs = base.clone();
        abs.unscaled = abs.unscaled.abs();
        let w = rscale + GUARD + f.max(0.0) as u32 + n.unsigned_abs().to_string().len() as u32;
        let t = ln_fixed(&abs, w) * BigInt::from(n);
        let mut out = exp_fixed(&t, w, rscale)?;
        if neg {
            out.unscaled = -out.unscaled;
        }
        return Ok(out);
    }
    let k = u32::try_from(n.unsigned_abs()).map_err(|_| overflow())?;
    let num = base.unscaled.pow(k);
    let scale = base.scale * k;
    if n > 0 {
        return Ok(round_to(&num, scale, rscale));
    }
    // 1 / base^k = 10^scale / num.
    let neg = num.is_negative();
    let q = round_half_away(&(p10(scale + rscale)), &num.abs());
    Ok(Dec {
        unscaled: if neg { -q } else { q },
        scale: rscale,
    })
}

/// `scale()`, `min_scale()`, `trim_scale()` of a finite value.
fn scales(name: &str, x: &Dec) -> Result<bson::Bson> {
    let trailing = {
        let mut u = x.unscaled.clone();
        let mut t = 0u32;
        while t < x.scale && !u.is_zero() && (&u % BigInt::from(10u32)).is_zero() {
            u /= BigInt::from(10u32);
            t += 1;
        }
        if u.is_zero() {
            x.scale
        } else {
            t
        }
    };
    Ok(match name {
        "scale" => bson::Bson::Int32(x.scale as i32),
        "min_scale" => bson::Bson::Int32((x.scale - trailing) as i32),
        _ => crate::numeric::numeric_bson(&render(&Dec {
            unscaled: &x.unscaled / p10(trailing),
            scale: x.scale - trailing,
        })?),
    })
}

/// `numeric_send`: the binary wire form -- ndigits, weight, sign, dscale
/// (each a big-endian int16), then the base-10000 digit groups.
pub fn numeric_send(canonical: &str) -> Vec<u8> {
    let (ndigits, weight, sign, dscale, groups): (i16, i16, u16, i16, Vec<i64>) = match canonical {
        "NaN" => (0, 0, 0xC000, 0, Vec::new()),
        "Infinity" => (0, 0, 0xD000, 0, Vec::new()),
        "-Infinity" => (0, 0, 0xF000, 0, Vec::new()),
        other => match Dec::parse(other) {
            Some(d) => {
                let (w, g) = nbase(&d);
                let sign = if d.unscaled.is_negative() { 0x4000 } else { 0 };
                (g.len() as i16, w as i16, sign, d.scale as i16, g)
            }
            None => (0, 0, 0xC000, 0, Vec::new()),
        },
    };
    let mut out = Vec::with_capacity(8 + 2 * groups.len());
    out.extend_from_slice(&ndigits.to_be_bytes());
    out.extend_from_slice(&weight.to_be_bytes());
    out.extend_from_slice(&sign.to_be_bytes());
    out.extend_from_slice(&dscale.to_be_bytes());
    for g in groups {
        out.extend_from_slice(&(g as i16).to_be_bytes());
    }
    out
}

/// The functions this module answers for a numeric argument.
pub fn is_function(name: &str) -> bool {
    matches!(
        name,
        "sqrt"
            | "exp"
            | "ln"
            | "log"
            | "log10"
            | "power"
            | "pow"
            | "scale"
            | "min_scale"
            | "trim_scale"
    )
}

/// Non-finite inputs, as numeric.c's special-value branches answer them.
fn non_finite(name: &str, args: &[String]) -> Option<Result<String>> {
    let special = |s: &str| matches!(s, "NaN" | "Infinity" | "-Infinity");
    if !args.iter().any(|a| special(a)) {
        return None;
    }
    let ok = |s: &str| Some(Ok(s.to_string()));
    let sign = |s: &str| -> i32 {
        match s {
            "Infinity" => 1,
            "-Infinity" => -1,
            other => Dec::parse(other).map_or(0, |d| d.unscaled.signum().to_i32().unwrap_or(0)),
        }
    };
    let one = |s: &str| Dec::parse(s).is_some_and(|d| cmp(&d, &dec(1, 0)).is_eq());
    match (name, args) {
        ("sqrt", [a]) if a == "-Infinity" => Some(Err(power_error(
            "cannot take square root of a negative number",
        ))),
        ("sqrt" | "ln", [a]) if a == "-Infinity" && name == "ln" => {
            Some(Err(log_error("cannot take logarithm of a negative number")))
        }
        ("exp", [a]) if a == "-Infinity" => ok("0"),
        ("sqrt" | "exp" | "ln", [a]) => ok(a),
        ("log", [b, x]) => {
            if b == "NaN" || x == "NaN" {
                return ok("NaN");
            }
            let (s1, s2) = (sign(b), sign(x));
            if s1 < 0 || s2 < 0 {
                return Some(Err(log_error("cannot take logarithm of a negative number")));
            }
            if s1 == 0 || s2 == 0 {
                return Some(Err(log_error("cannot take logarithm of zero")));
            }
            if b == "Infinity" {
                return ok(if x == "Infinity" { "NaN" } else { "0" });
            }
            ok("Infinity")
        }
        ("power", [x, y]) => {
            if x == "NaN" {
                return ok(if !special(y) && sign(y) == 0 {
                    "1"
                } else {
                    "NaN"
                });
            }
            if y == "NaN" {
                return ok(if !special(x) && one(x) { "1" } else { "NaN" });
            }
            let (s1, s2) = (sign(x), sign(y));
            if s1 == 0 && s2 < 0 {
                return Some(Err(power_error(
                    "zero raised to a negative power is undefined",
                )));
            }
            let y_integral = special(y) || Dec::parse(y).is_some_and(|d| is_integral(&d));
            if s1 < 0 && !y_integral {
                return Some(Err(power_error(
                    "a negative number raised to a non-integer power yields a complex result",
                )));
            }
            if !special(x) && one(x) {
                return ok("1");
            }
            if s2 == 0 {
                return ok("1");
            }
            if s1 == 0 && s2 > 0 {
                return ok("0");
            }
            if special(y) {
                let abs_gt_one = special(x)
                    || Dec::parse(x).is_some_and(|d| {
                        let mut a = d;
                        if cmp(&a, &dec(-1, 0)).is_eq() {
                            return false;
                        }
                        a.unscaled = a.unscaled.abs();
                        cmp(&a, &dec(1, 0)).is_gt()
                    });
                if !special(x) && Dec::parse(x).is_some_and(|d| cmp(&d, &dec(-1, 0)).is_eq()) {
                    return ok("1");
                }
                return ok(if abs_gt_one == (s2 > 0) {
                    "Infinity"
                } else {
                    "0"
                });
            }
            if x == "Infinity" {
                return ok(if s2 > 0 { "Infinity" } else { "0" });
            }
            // x is -Infinity.
            if s2 < 0 {
                return ok("0");
            }
            let odd = Dec::parse(y).is_some_and(|d| {
                is_integral(&d) && (&d.unscaled / p10(d.scale)) % 2 != BigInt::zero()
            });
            ok(if odd { "-Infinity" } else { "Infinity" })
        }
        _ => None,
    }
}

/// Evaluate `name` over canonical numeric texts. `None` for a name this
/// module does not answer.
pub fn call(name: &str, args: &[String]) -> Option<Result<bson::Bson>> {
    let name = match name {
        "pow" => "power",
        "log10" => "log",
        other => other,
    };
    // `log(x)` is `log(10, x)` in pg_proc.
    let owned;
    let args: &[String] = if name == "log" && args.len() == 1 {
        owned = vec!["10".to_string(), args[0].clone()];
        &owned
    } else {
        args
    };
    if matches!(name, "scale" | "min_scale" | "trim_scale") {
        let [a] = args else { return None };
        return match Dec::parse(a) {
            Some(d) => Some(scales(name, &d)),
            None => Some(Ok(if name == "trim_scale" {
                crate::numeric::numeric_bson(a)
            } else {
                bson::Bson::Null
            })),
        };
    }
    if let Some(r) = non_finite(name, args) {
        return Some(r.map(|t| crate::numeric::numeric_bson(&t)));
    }
    let decs: Option<Vec<Dec>> = args.iter().map(|a| Dec::parse(a)).collect();
    let decs = decs?;
    let out = match (name, decs.as_slice()) {
        ("sqrt", [x]) => sqrt(x),
        ("exp", [x]) => exp(x),
        ("ln", [x]) => ln(x),
        ("log", [b, x]) => log(b, x),
        ("power", [b, e]) => power(b, e),
        _ => return None,
    };
    Some(out.map(|t| crate::numeric::numeric_bson(&t)))
}

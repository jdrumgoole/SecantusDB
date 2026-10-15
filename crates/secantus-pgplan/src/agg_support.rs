//! The aggregates' transition, combine and final functions, called by name
//! over their state arrays, as PostgreSQL 15.19 answers them (float.c,
//! numeric.c's `int*_avg_accum`, timestamp.c's `interval_accum`):
//! `float8_accum('{1,2,0}', 3)` is `{2,5,0.5}`.
//!
//! The float8 updates are the server's own aggregates' arithmetic
//! (Youngs-Cramer, with the multiply-adds fused where the reference build
//! fuses them), one row at a time. A state of the wrong length is refused
//! as PostgreSQL refuses it (`float8_accum: expected 3-element float8
//! array`, XX000).

use super::*;

/// The result type of `name` over `nargs` arguments, when it is one of these.
pub(crate) fn result_type(name: &str, nargs: usize) -> Option<&'static str> {
    Some(match (name, nargs) {
        ("float4_accum" | "float8_accum" | "float8_combine", 2) => "float8[]",
        ("float8_regr_accum", 3) | ("float8_regr_combine", 2) => "float8[]",
        (
            "float8_avg"
            | "float8_var_pop"
            | "float8_var_samp"
            | "float8_stddev_pop"
            | "float8_stddev_samp"
            | "float8_regr_sxx"
            | "float8_regr_syy"
            | "float8_regr_sxy"
            | "float8_regr_avgx"
            | "float8_regr_avgy"
            | "float8_regr_r2"
            | "float8_regr_slope"
            | "float8_regr_intercept"
            | "float8_covar_pop"
            | "float8_covar_samp"
            | "float8_corr",
            1,
        ) => "float8",
        (
            "int2_avg_accum" | "int2_avg_accum_inv" | "int4_avg_accum" | "int4_avg_accum_inv"
            | "int4_avg_combine",
            2,
        ) => "int8[]",
        ("int8_avg", 1) => "numeric",
        ("int2int4_sum", 1) => "int8",
        ("interval_accum" | "interval_accum_inv" | "interval_combine", 2) => "interval[]",
        ("interval_avg", 1) => "interval",
        ("range_intersect_agg_transfn", 2) => "anyrange",
        ("multirange_intersect_agg_transfn", 2) => "anymultirange",
        _ => return None,
    })
}

fn internal(message: String) -> Error {
    Error::Sqlstate("XX000", message)
}

fn floats(name: &str, v: &Bson, len: usize) -> Result<Vec<f64>> {
    let bad = || internal(format!("{name}: expected {len}-element float8 array"));
    let Bson::Array(items) = arrays::strip(v) else {
        return Err(bad());
    };
    if items.len() != len {
        return Err(bad());
    }
    items
        .iter()
        .map(|i| match i {
            Bson::Double(d) => Ok(*d),
            Bson::Int32(n) => Ok(f64::from(*n)),
            Bson::Int64(n) => Ok(*n as f64),
            _ => Err(bad()),
        })
        .collect()
}

fn ints(v: &Bson) -> Result<(i64, i64)> {
    let bad = || internal("expected 2-element int8 array".into());
    let Bson::Array(items) = arrays::strip(v) else {
        return Err(bad());
    };
    let n = |i: &Bson| match i {
        Bson::Int64(n) => Ok(*n),
        Bson::Int32(n) => Ok(i64::from(*n)),
        _ => Err(bad()),
    };
    match items.as_slice() {
        [count, sum] => Ok((n(count)?, n(sum)?)),
        _ => Err(bad()),
    }
}

fn intervals(v: &Bson) -> Result<(Bson, Interval)> {
    let bad = || internal("expected 2-element interval array".into());
    let Bson::Array(items) = arrays::strip(v) else {
        return Err(bad());
    };
    match items.as_slice() {
        [sum, n] => {
            let n = Interval::from_bson(n)
                .or_else(|| {
                    cast_value(n.clone(), "interval")
                        .ok()
                        .and_then(|v| Interval::from_bson(&v))
                })
                .ok_or_else(bad)?;
            Ok((sum.clone(), n))
        }
        _ => Err(bad()),
    }
}

fn float_array(values: &[f64]) -> Bson {
    Bson::Array(values.iter().map(|v| Bson::Double(*v)).collect())
}

/// An expression over `params`, by the planner's own operators.
fn eval(sql: &str, params: &[Bson]) -> Result<Bson> {
    let N::SelectStmt(s) = parse_one(&format!("SELECT {sql}"))? else {
        return Err(Error::Internal("aggregate support expression".into()));
    };
    let Some(N::ResTarget(rt)) = s.target_list.first().and_then(|t| t.node.as_ref()) else {
        return Err(Error::Internal("aggregate support expression".into()));
    };
    let node = rt
        .val
        .as_deref()
        .ok_or_else(|| Error::Internal("aggregate support expression".into()))?;
    const_value(node, params)
}

/// `float8_accum`'s one row.
fn accum(s: &[f64], v: f64) -> Bson {
    let (prev_n, mut sxx) = (s[0], s[2]);
    let n = prev_n + 1.0;
    let sx = s[1] + v;
    if prev_n > 0.0 {
        let tmp = v.mul_add(n, -sx);
        sxx += tmp * tmp / (n * prev_n);
        if sx.is_infinite() || sxx.is_infinite() {
            sxx = f64::NAN;
        }
    } else if v.is_nan() || v.is_infinite() {
        sxx = f64::NAN;
    }
    float_array(&[n, sx, sxx])
}

/// `float8_regr_accum`'s one row of `(y, x)`.
fn regr_accum(s: &[f64], y: f64, x: f64) -> Bson {
    let prev_n = s[0];
    let n = prev_n + 1.0;
    let (sx, sy) = (s[1] + x, s[3] + y);
    let (mut sxx, mut syy, mut sxy) = (s[2], s[4], s[5]);
    if prev_n > 0.0 {
        let tmp_x = x.mul_add(n, -sx);
        let tmp_y = y.mul_add(n, -sy);
        let scale = 1.0 / (n * prev_n);
        sxx = (tmp_x * tmp_x).mul_add(scale, sxx);
        syy = (tmp_y * tmp_y).mul_add(scale, syy);
        sxy = (tmp_x * tmp_y).mul_add(scale, sxy);
    } else {
        if x.is_nan() || x.is_infinite() {
            sxx = f64::NAN;
            sxy = f64::NAN;
        }
        if y.is_nan() || y.is_infinite() {
            syy = f64::NAN;
            sxy = f64::NAN;
        }
    }
    float_array(&[n, sx, sxx, sy, syy, sxy])
}

fn combine(a: &[f64], b: &[f64]) -> Bson {
    if a[0] == 0.0 {
        return float_array(b);
    }
    if b[0] == 0.0 {
        return float_array(a);
    }
    let n = a[0] + b[0];
    let tmp = a[1] / a[0] - b[1] / b[0];
    let sxx = a[2] + b[2] + a[0] * b[0] * tmp * tmp / n;
    float_array(&[n, a[1] + b[1], sxx])
}

fn regr_combine(a: &[f64], b: &[f64]) -> Bson {
    if a[0] == 0.0 {
        return float_array(b);
    }
    if b[0] == 0.0 {
        return float_array(a);
    }
    let n = a[0] + b[0];
    let tmp1 = a[1] / a[0] - b[1] / b[0];
    let tmp2 = a[3] / a[0] - b[3] / b[0];
    let sxx = a[2] + b[2] + a[0] * b[0] * tmp1 * tmp1 / n;
    let syy = a[4] + b[4] + a[0] * b[0] * tmp2 * tmp2 / n;
    let sxy = a[5] + b[5] + a[0] * b[0] * tmp1 * tmp2 / n;
    float_array(&[n, a[1] + b[1], sxx, a[3] + b[3], syy, sxy])
}

fn double(v: Option<f64>) -> Bson {
    v.map_or(Bson::Null, Bson::Double)
}

/// The variance family's final functions over `(N, Sx, Sxx)`.
fn variance(name: &str, s: &[f64]) -> Bson {
    let (n, sx, sxx) = (s[0], s[1], s[2]);
    double(match name {
        "float8_avg" => (n != 0.0).then(|| sx / n),
        "float8_var_pop" => (n != 0.0).then(|| sxx / n),
        "float8_var_samp" => (n > 1.0).then(|| sxx / (n - 1.0)),
        "float8_stddev_pop" => (n != 0.0).then(|| (sxx / n).sqrt()),
        _ => (n > 1.0).then(|| (sxx / (n - 1.0)).sqrt()),
    })
}

/// The regression family's final functions over `(N, Sx, Sxx, Sy, Syy, Sxy)`.
fn regression(name: &str, s: &[f64]) -> Bson {
    let (n, sx, sxx, sy, syy, sxy) = (s[0], s[1], s[2], s[3], s[4], s[5]);
    if n < 1.0 {
        return Bson::Null;
    }
    double(match name {
        "float8_regr_sxx" => Some(sxx),
        "float8_regr_syy" => Some(syy),
        "float8_regr_sxy" => Some(sxy),
        "float8_regr_avgx" => Some(sx / n),
        "float8_regr_avgy" => Some(sy / n),
        "float8_covar_pop" => Some(sxy / n),
        "float8_covar_samp" => (n >= 2.0).then(|| sxy / (n - 1.0)),
        "float8_corr" => (sxx != 0.0 && syy != 0.0).then(|| sxy / (sxx * syy).sqrt()),
        "float8_regr_r2" if sxx == 0.0 => None,
        "float8_regr_r2" if syy == 0.0 => Some(1.0),
        "float8_regr_r2" => Some((sxy * sxy) / (sxx * syy)),
        "float8_regr_slope" => (sxx != 0.0).then(|| sxy / sxx),
        _ => (sxx != 0.0).then(|| (sy - sx * sxy / sxx) / n),
    })
}

fn number(v: &Bson) -> Option<f64> {
    match v {
        Bson::Double(d) => Some(*d),
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        other => value_text(other).parse().ok(),
    }
}

fn int_of(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        other => value_text(other).parse().ok(),
    }
}

/// `name(args)`, when it is one of these; every one is strict.
pub(crate) fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    result_type(name, args.len())?;
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    Some(match name {
        "float4_accum" | "float8_accum" => floats(name, &args[0], 3).and_then(|s| {
            let v = number(&args[1]).ok_or_else(|| Error::Internal(name.into()))?;
            Ok(accum(&s, v))
        }),
        "float8_combine" => {
            floats(name, &args[0], 3).and_then(|a| Ok(combine(&a, &floats(name, &args[1], 3)?)))
        }
        "float8_regr_accum" => floats(name, &args[0], 6).and_then(|s| {
            let y = number(&args[1]).ok_or_else(|| Error::Internal(name.into()))?;
            let x = number(&args[2]).ok_or_else(|| Error::Internal(name.into()))?;
            Ok(regr_accum(&s, y, x))
        }),
        "float8_regr_combine" => floats(name, &args[0], 6)
            .and_then(|a| Ok(regr_combine(&a, &floats(name, &args[1], 6)?))),
        "float8_avg" | "float8_var_pop" | "float8_var_samp" | "float8_stddev_pop"
        | "float8_stddev_samp" => floats(name, &args[0], 3).map(|s| variance(name, &s)),
        n if n.starts_with("float8_regr_") || n.starts_with("float8_co") => {
            floats(name, &args[0], 6).map(|s| regression(name, &s))
        }
        "int2_avg_accum" | "int2_avg_accum_inv" | "int4_avg_accum" | "int4_avg_accum_inv" => {
            ints(&args[0]).and_then(|(count, sum)| {
                let v = int_of(&args[1]).ok_or_else(|| Error::Internal(name.into()))?;
                let (count, sum) = if name.ends_with("_inv") {
                    (count - 1, sum - v)
                } else {
                    (count + 1, sum + v)
                };
                Ok(Bson::Array(vec![Bson::Int64(count), Bson::Int64(sum)]))
            })
        }
        "int4_avg_combine" => Err(internal(
            "aggregate function called in non-aggregate context".into(),
        )),
        "int8_avg" => ints(&args[0]).and_then(|(count, sum)| {
            if count == 0 {
                return Ok(Bson::Null);
            }
            eval(
                "$1::numeric / $2::numeric",
                &[Bson::Int64(sum), Bson::Int64(count)],
            )
        }),
        "int2int4_sum" => ints(&args[0]).map(|(count, sum)| {
            if count == 0 {
                Bson::Null
            } else {
                Bson::Int64(sum)
            }
        }),
        "interval_accum" | "interval_accum_inv" => intervals(&args[0]).and_then(|(sum, n)| {
            let (op, step) = if name.ends_with("_inv") {
                ("-", -1)
            } else {
                ("+", 1)
            };
            let sum = eval(
                &format!("$1::interval {op} $2::interval"),
                &[sum, args[1].clone()],
            )?;
            let n = Interval {
                micros: n.micros + step,
                ..n
            };
            Ok(Bson::Array(vec![sum, n.to_bson()]))
        }),
        "interval_combine" => intervals(&args[0]).and_then(|(s1, n1)| {
            let (s2, n2) = intervals(&args[1])?;
            let sum = eval("$1::interval + $2::interval", &[s1, s2])?;
            let n = Interval {
                micros: n1.micros + n2.micros,
                ..n1
            };
            Ok(Bson::Array(vec![sum, n.to_bson()]))
        }),
        "interval_avg" => intervals(&args[0]).and_then(|(sum, n)| {
            if n.micros == 0 {
                return Ok(Bson::Null);
            }
            eval(
                "$1::interval / $2::float8",
                &[sum, Bson::Double(n.micros as f64)],
            )
        }),
        _ => Err(internal(format!("{name} called in non-aggregate context"))),
    })
}

/// `in_range(val, base, offset, sub, less)`: a window frame's
/// `offset PRECEDING` / `FOLLOWING` test, whether `val` lies on the near
/// side of `base -/+ offset` (`in_range_int4_int4`, `in_range_float8_float8`,
/// `in_range_numeric_numeric`, `in_range_time_interval`, ...).
pub(crate) fn in_range(args: &[Bson], types: &[String]) -> Option<Result<Bson>> {
    if args.len() != 5 {
        return None;
    }
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    let t = |i: usize| types.get(i).map_or("", String::as_str);
    let bad_size = || {
        Error::Sqlstate(
            "22013",
            "invalid preceding or following size in window function".into(),
        )
    };
    let (sub, less) = match (&args[3], &args[4]) {
        (Bson::Boolean(s), Bson::Boolean(l)) => (*s, *l),
        _ => return None,
    };
    let near = |val: f64, sum: f64| if less { val <= sum } else { val >= sum };
    Some((|| -> Result<Bson> {
        Ok(Bson::Boolean(match (t(0), t(2)) {
            ("int2" | "int4" | "int8", "int2" | "int4" | "int8") => {
                let n = |v: &Bson| -> Result<i128> {
                    value_text(v)
                        .trim()
                        .parse()
                        .map_err(|_| Error::Internal("in_range".into()))
                };
                let (val, base, offset) = (n(&args[0])?, n(&args[1])?, n(&args[2])?);
                if offset < 0 {
                    return Err(bad_size());
                }
                let sum = if sub { base - offset } else { base + offset };
                if less {
                    val <= sum
                } else {
                    val >= sum
                }
            }
            ("float4" | "float8", "float8") => {
                let f = |v: &Bson| number(v).ok_or_else(|| Error::Internal("in_range".into()));
                let (val, base, offset) = (f(&args[0])?, f(&args[1])?, f(&args[2])?);
                if offset.is_nan() || offset < 0.0 {
                    return Err(bad_size());
                }
                if val.is_nan() {
                    base.is_nan() || !less
                } else if base.is_nan() {
                    less
                } else if offset.is_infinite()
                    && base.is_infinite()
                    && (if sub { base > 0.0 } else { base < 0.0 })
                {
                    true
                } else {
                    near(val, if sub { base - offset } else { base + offset })
                }
            }
            ("numeric", "numeric") => {
                let text = |v: &Bson| value_text(v).trim().to_string();
                let (val, base, offset) = (text(&args[0]), text(&args[1]), text(&args[2]));
                let negative = matches!(
                    eval("$1::numeric < 0", &[args[2].clone()])?,
                    Bson::Boolean(true)
                );
                if offset == "NaN" || negative {
                    return Err(bad_size());
                }
                let infinite = |s: &str| s.ends_with("Infinity");
                if val == "NaN" {
                    base == "NaN" || !less
                } else if base == "NaN" {
                    less
                } else if infinite(&offset)
                    && infinite(&base)
                    && (if sub {
                        !base.starts_with('-')
                    } else {
                        base.starts_with('-')
                    })
                {
                    true
                } else {
                    let op = if sub { "-" } else { "+" };
                    let cmp = if less { "<=" } else { ">=" };
                    matches!(
                        eval(
                            &format!("$1::numeric {cmp} ($2::numeric {op} $3::numeric)"),
                            &[args[0].clone(), args[1].clone(), args[2].clone()]
                        )?,
                        Bson::Boolean(true)
                    )
                }
            }
            (
                ty @ ("date" | "timestamp" | "timestamptz" | "interval" | "time" | "timetz"),
                "interval",
            ) => {
                let offset = Interval::from_bson(&cast_value(args[2].clone(), "interval")?)
                    .ok_or_else(|| Error::Internal("in_range".into()))?;
                let negative = if matches!(ty, "time" | "timetz") {
                    offset.micros < 0
                } else {
                    offset.comparable_micros() < 0
                };
                if negative {
                    return Err(bad_size());
                }
                // `time` / `timetz` add the offset's time part only, and do
                // not wrap round midnight.
                if ty == "time" {
                    let val = crate::hashfn::time_micros(&args[0])?;
                    let base = crate::hashfn::time_micros(&args[1])?;
                    let sum = if sub {
                        base - offset.micros
                    } else {
                        base + offset.micros
                    };
                    return Ok(Bson::Boolean(if less { val <= sum } else { val >= sum }));
                }
                if ty == "timetz" {
                    // `timetz_cmp`: by the instant, then by the zone.
                    let (vt, vz) = crate::hashfn::timetz_parts(&args[0])?;
                    let (bt, bz) = crate::hashfn::timetz_parts(&args[1])?;
                    let st = if sub {
                        bt - offset.micros
                    } else {
                        bt + offset.micros
                    };
                    let key = |t: i64, z: i32| (t + i64::from(z) * 1_000_000, z);
                    let (v, s) = (key(vt, vz), key(st, bz));
                    return Ok(Bson::Boolean(if less { v <= s } else { v >= s }));
                }
                let op = if sub { "-" } else { "+" };
                let cmp = if less { "<=" } else { ">=" };
                let sql = match ty {
                    "date" => {
                        format!("$1::date::timestamp {cmp} ($2::date::timestamp {op} $3::interval)")
                    }
                    _ => format!("$1::{ty} {cmp} ($2::{ty} {op} $3::interval)"),
                };
                matches!(
                    eval(&sql, &[args[0].clone(), args[1].clone(), args[2].clone()])?,
                    Bson::Boolean(true)
                )
            }
            _ => return Err(Error::Unsupported(format!("in_range({}, ...)", t(0)))),
        }))
    })())
}

//! The aggregates beyond the basic family: variance and standard deviation,
//! the float8 regression family, `json[b]_agg` / `json[b]_object_agg`,
//! `bit_and` / `bit_or`, and the ordered-set (`percentile_cont`,
//! `percentile_disc`, `mode`) and hypothetical-set (`rank`, `dense_rank`,
//! `percent_rank`, `cume_dist` `WITHIN GROUP`) aggregates.
//!
//! Each transcribes PostgreSQL 14's accumulator and final function rather
//! than a textbook formula, because the digits are the answer a client
//! compares: `float8_accum` / `float8_regr_accum` are the Youngs-Cramer
//! update (not the naive sum of squares, which rounds differently), and an
//! integer or numeric `variance` is EXACT numeric at `select_div_scale`.

use std::cmp::Ordering;

use bson::{Bson, Document};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use secantus_pgplan::json::Json;
use secantus_pgplan::{AggFunc, AggItem, OrderKey};

fn user_error(code: &str, msg: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new("ERROR".into(), code.into(), msg)))
}

fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        Bson::Boolean(_) | Bson::Null => None,
        other => secantus_pgplan::numeric::numeric_text(other)
            .or_else(|| match other {
                Bson::String(s) => Some(s.clone()),
                _ => None,
            })
            .and_then(|t| t.trim().parse::<f64>().ok()),
    }
}

fn float_bson(v: f64) -> Bson {
    Bson::Double(v)
}

fn numeric_bson(text: &str) -> Bson {
    secantus_pgplan::numeric::numeric_bson(text)
}

/// Is this one of the aggregates computed here?
pub(crate) fn is_extended(f: AggFunc) -> bool {
    !matches!(
        f,
        AggFunc::CountStar
            | AggFunc::Count
            | AggFunc::Sum
            | AggFunc::Min
            | AggFunc::Max
            | AggFunc::ArrayAgg
            | AggFunc::BoolAnd
            | AggFunc::BoolOr
            | AggFunc::Avg
            | AggFunc::StringAgg
    )
}

/// Compute one of the extended aggregates over the group's rows, FILTER
/// and ordering already applied by the caller.
pub(crate) fn compute(item: &AggItem, rows: &[Document]) -> PgWireResult<Bson> {
    let field = item.field.as_deref().unwrap_or("");
    let get = |d: &Document, f: &str| d.get(f).cloned().unwrap_or(Bson::Null);
    let non_null: Vec<Bson> = rows
        .iter()
        .map(|d| get(d, field))
        .filter(|v| *v != Bson::Null)
        .collect();
    match item.func {
        AggFunc::VarSamp | AggFunc::VarPop | AggFunc::StddevSamp | AggFunc::StddevPop => {
            let sample = matches!(item.func, AggFunc::VarSamp | AggFunc::StddevSamp);
            let stddev = matches!(item.func, AggFunc::StddevSamp | AggFunc::StddevPop);
            let float = matches!(
                item.source_type.as_deref(),
                Some("float4" | "float8" | "real" | "double precision")
            );
            if float {
                let (n, _, sxx) = float8_accum(non_null.iter().filter_map(as_f64));
                if n == 0.0 || (sample && n <= 1.0) {
                    return Ok(Bson::Null);
                }
                let var = sxx / if sample { n - 1.0 } else { n };
                return Ok(float_bson(if stddev { var.sqrt() } else { var }));
            }
            let texts: Vec<String> = non_null
                .iter()
                .filter_map(|v| match v {
                    Bson::Int32(i) => Some(i.to_string()),
                    Bson::Int64(i) => Some(i.to_string()),
                    other => secantus_pgplan::numeric::numeric_text(other),
                })
                .collect();
            Ok(
                secantus_pgplan::numeric::numeric_variance(&texts, sample, stddev)
                    .map_or(Bson::Null, |t| numeric_bson(&t)),
            )
        }
        AggFunc::Corr
        | AggFunc::CovarPop
        | AggFunc::CovarSamp
        | AggFunc::RegrCount
        | AggFunc::RegrAvgX
        | AggFunc::RegrAvgY
        | AggFunc::RegrSxx
        | AggFunc::RegrSyy
        | AggFunc::RegrSxy
        | AggFunc::RegrSlope
        | AggFunc::RegrIntercept
        | AggFunc::RegrR2 => {
            let field2 = item.field2.as_deref().unwrap_or("");
            // `corr(y, x)`: the FIRST argument is the dependent variable.
            let pairs = rows.iter().filter_map(|d| {
                let y = as_f64(&get(d, field))?;
                let x = as_f64(&get(d, field2))?;
                Some((y, x))
            });
            let a = regr_accum(pairs);
            Ok(regr_final(item.func, &a))
        }
        AggFunc::JsonAgg | AggFunc::JsonbAgg => {
            if rows.is_empty() {
                return Ok(Bson::Null);
            }
            let ty = item.source_type.as_deref().unwrap_or("text");
            let items: Vec<Json> = rows.iter().map(|d| to_json(&get(d, field), ty)).collect();
            Ok(Bson::String(if item.func == AggFunc::JsonbAgg {
                secantus_pgplan::json::render_jsonb(&Json::Array(items))
            } else {
                let parts: Vec<String> =
                    rows.iter().map(|d| json_text(&get(d, field), ty)).collect();
                format!("[{}]", parts.join(", "))
            }))
        }
        AggFunc::JsonObjectAgg | AggFunc::JsonbObjectAgg => {
            if rows.is_empty() {
                return Ok(Bson::Null);
            }
            let field2 = item.field2.as_deref().unwrap_or("");
            let vty = item.source_type2.as_deref().unwrap_or("text");
            let mut pairs: Vec<(String, Bson)> = Vec::new();
            for d in rows {
                let k = get(d, field);
                if k == Bson::Null {
                    return Err(user_error("22023", "field name must not be null".into()));
                }
                pairs.push((secantus_pgplan::value_text(&k), get(d, field2)));
            }
            Ok(Bson::String(if item.func == AggFunc::JsonbObjectAgg {
                let obj = Json::Object(
                    pairs
                        .iter()
                        .map(|(k, v)| (k.clone(), to_json(v, vty)))
                        .collect(),
                );
                secantus_pgplan::json::render_jsonb(&obj)
            } else {
                let parts: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{} : {}",
                            secantus_pgplan::json::render_jsonb(&Json::Str(k.clone())),
                            json_text(v, vty)
                        )
                    })
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }))
        }
        AggFunc::RangeAgg | AggFunc::RangeIntersectAgg => {
            let ty = item.source_type.as_deref().unwrap_or_default();
            let out = if item.func == AggFunc::RangeAgg {
                secantus_pgplan::range_ops::range_agg(&non_null, ty)
            } else {
                secantus_pgplan::range_ops::range_intersect_agg(&non_null, ty)
            };
            out.map_err(|e| crate::PgHandler::err(&e))
        }
        // A CREATE AGGREGATE aggregate folds every row's value, NULLs
        // included: whether a NULL reaches the state function is its
        // strictness's call, not ours.
        AggFunc::User => {
            let Some(agg) = item.user.as_ref() else {
                return Ok(Bson::Null);
            };
            let values: Vec<Bson> = rows.iter().map(|d| get(d, field)).collect();
            secantus_pgplan::user_agg::compute(agg, &values, item.source_type.as_deref())
                .map_err(|e| crate::PgHandler::err(&e))
        }
        AggFunc::BitAnd | AggFunc::BitOr => {
            let ints: Vec<i64> = non_null
                .iter()
                .filter_map(|v| match v {
                    Bson::Int32(i) => Some(i64::from(*i)),
                    Bson::Int64(i) => Some(*i),
                    _ => None,
                })
                .collect();
            let Some(first) = ints.first() else {
                return Ok(Bson::Null);
            };
            let r = ints.iter().skip(1).fold(*first, |a, b| {
                if item.func == AggFunc::BitAnd {
                    a & b
                } else {
                    a | b
                }
            });
            Ok(match non_null.first() {
                Some(Bson::Int32(_)) => Bson::Int32(r as i32),
                _ => Bson::Int64(r),
            })
        }
        AggFunc::PercentileCont | AggFunc::PercentileDisc | AggFunc::Mode => {
            // The WITHIN GROUP values, NULLs skipped, already in the written
            // order (the caller sorted by it).
            let values = non_null;
            if item.func == AggFunc::Mode {
                return Ok(mode(&values));
            }
            let Some(direct) = item.direct.first() else {
                return Ok(Bson::Null);
            };
            let one = |p: &Bson| -> PgWireResult<Bson> {
                let Some(frac) = as_f64(p) else {
                    return Ok(Bson::Null);
                };
                if !(0.0..=1.0).contains(&frac) || frac.is_nan() {
                    return Err(user_error(
                        "22003",
                        format!("percentile value {} is not between 0 and 1", g_format(frac)),
                    ));
                }
                if values.is_empty() {
                    return Ok(Bson::Null);
                }
                Ok(if item.func == AggFunc::PercentileCont {
                    percentile_cont(&values, frac)
                } else {
                    let n = values.len();
                    let row = (frac * n as f64).ceil() as usize;
                    values[row.max(1) - 1].clone()
                })
            };
            match direct {
                Bson::Null => Ok(Bson::Null),
                Bson::Array(fracs) => {
                    if values.is_empty() {
                        return Ok(Bson::Null);
                    }
                    Ok(Bson::Array(
                        fracs.iter().map(one).collect::<PgWireResult<_>>()?,
                    ))
                }
                p => one(p),
            }
        }
        AggFunc::HypRank
        | AggFunc::HypDenseRank
        | AggFunc::HypPercentRank
        | AggFunc::HypCumeDist => {
            let hypo = item.direct.first().cloned().unwrap_or(Bson::Null);
            let key = item.order.first();
            let n = rows.len();
            let cmp_to_hypo = |v: &Bson| compare_with_key(v, &hypo, key);
            let before = rows
                .iter()
                .filter(|d| cmp_to_hypo(&get(d, field)) == Ordering::Less)
                .count();
            Ok(match item.func {
                AggFunc::HypRank => Bson::Int64(before as i64 + 1),
                AggFunc::HypDenseRank => {
                    let mut distinct: Vec<Bson> = Vec::new();
                    for d in rows {
                        let v = get(d, field);
                        if cmp_to_hypo(&v) == Ordering::Less
                            && !distinct
                                .iter()
                                .any(|x| compare_with_key(x, &v, key) == Ordering::Equal)
                        {
                            distinct.push(v);
                        }
                    }
                    Bson::Int64(distinct.len() as i64 + 1)
                }
                AggFunc::HypPercentRank => {
                    if n == 0 {
                        float_bson(0.0)
                    } else {
                        float_bson(before as f64 / n as f64)
                    }
                }
                _ => {
                    let upto = rows
                        .iter()
                        .filter(|d| cmp_to_hypo(&get(d, field)) != Ordering::Greater)
                        .count();
                    float_bson((upto + 1) as f64 / (n + 1) as f64)
                }
            })
        }
        _ => Ok(Bson::Null),
    }
}

/// `%g`, as the percentile error message prints the fraction.
fn g_format(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A value against the hypothetical one, in the WITHIN GROUP key's order.
fn compare_with_key(a: &Bson, b: &Bson, key: Option<&OrderKey>) -> Ordering {
    let (asc, nulls_first) = match key {
        Some(k) => (k.ascending, k.nulls == secantus_pgplan::Nulls::First),
        None => (true, false),
    };
    match (*a == Bson::Null, *b == Bson::Null) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, true) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, false) => {
            let c = secantus_pgplan::compare_values(a, b).unwrap_or(Ordering::Equal);
            if asc {
                c
            } else {
                c.reverse()
            }
        }
    }
}

/// `percentile_cont`: linear interpolation between the two rows around
/// `frac * (n - 1)` (`float8_lerp`).
fn percentile_cont(values: &[Bson], frac: f64) -> Bson {
    let n = values.len();
    let pos = frac * (n - 1) as f64;
    let first = pos.floor() as usize;
    let second = pos.ceil() as usize;
    let lo = as_f64(&values[first]).unwrap_or(0.0);
    if first == second {
        return float_bson(lo);
    }
    let hi = as_f64(&values[second]).unwrap_or(0.0);
    // `proportion = percentile * (n - 1) - first_row` and `float8_lerp`'s
    // `lo + pct * (hi - lo)`, each a FUSED multiply-add: the reference
    // server is an arm64 clang build, which contracts both, and only the
    // fused form reproduces its last digit (0.9 over 4 rows gives
    // -3.5000000000000004, not -3.500000000000001). An x86 build without FMA
    // rounds each step separately.
    let proportion = frac.mul_add((n - 1) as f64, -(first as f64));
    float_bson(proportion.mul_add(hi - lo, lo))
}

/// `mode_final`: the most frequent value, the first such in sort order.
fn mode(values: &[Bson]) -> Bson {
    let mut best: Option<(&Bson, usize)> = None;
    let mut i = 0;
    while i < values.len() {
        let mut j = i + 1;
        while j < values.len()
            && secantus_pgplan::compare_values(&values[i], &values[j]) == Some(Ordering::Equal)
        {
            j += 1;
        }
        let freq = j - i;
        if best.is_none_or(|(_, f)| freq > f) {
            best = Some((&values[i], freq));
        }
        i = j;
    }
    best.map_or(Bson::Null, |(v, _)| v.clone())
}

/// `float8_accum`: `(N, Sx, Sxx)` by the Youngs-Cramer update.
fn float8_accum(values: impl Iterator<Item = f64>) -> (f64, f64, f64) {
    let (mut n, mut sx, mut sxx) = (0.0f64, 0.0f64, 0.0f64);
    for v in values {
        let prev_n = n;
        n += 1.0;
        sx += v;
        if prev_n > 0.0 {
            // `newval * N - Sx` is a fused multiply-add in the reference
            // (arm64 clang) build; the division that follows is not.
            let tmp = v.mul_add(n, -sx);
            sxx += tmp * tmp / (n * prev_n);
            if sxx.is_infinite() && !v.is_infinite() {
                // PostgreSQL reports an overflow here; keep the value.
            }
        } else if v.is_nan() || v.is_infinite() {
            sxx = f64::NAN;
        }
    }
    (n, sx, sxx)
}

/// `float8_regr_accum`'s state: N, Sx, Sxx, Sy, Syy, Sxy.
struct Regr {
    n: f64,
    sx: f64,
    sxx: f64,
    sy: f64,
    syy: f64,
    sxy: f64,
}

fn regr_accum(pairs: impl Iterator<Item = (f64, f64)>) -> Regr {
    let mut a = Regr {
        n: 0.0,
        sx: 0.0,
        sxx: 0.0,
        sy: 0.0,
        syy: 0.0,
        sxy: 0.0,
    };
    for (y, x) in pairs {
        let prev_n = a.n;
        a.n += 1.0;
        a.sx += x;
        a.sy += y;
        if prev_n > 0.0 {
            // Each update as the reference build computes it: the
            // multiply-adds fused (arm64 clang contracts them).
            let tmp_x = x.mul_add(a.n, -a.sx);
            let tmp_y = y.mul_add(a.n, -a.sy);
            let scale = 1.0 / (a.n * prev_n);
            a.sxx = (tmp_x * tmp_x).mul_add(scale, a.sxx);
            a.syy = (tmp_y * tmp_y).mul_add(scale, a.syy);
            a.sxy = (tmp_x * tmp_y).mul_add(scale, a.sxy);
        } else {
            if x.is_nan() || x.is_infinite() {
                a.sxx = f64::NAN;
                a.sxy = f64::NAN;
            }
            if y.is_nan() || y.is_infinite() {
                a.syy = f64::NAN;
                a.sxy = f64::NAN;
            }
        }
    }
    a
}

fn regr_final(f: AggFunc, a: &Regr) -> Bson {
    if f == AggFunc::RegrCount {
        return Bson::Int64(a.n as i64);
    }
    if a.n < 1.0 {
        return Bson::Null;
    }
    let v = match f {
        AggFunc::RegrSxx => a.sxx,
        AggFunc::RegrSyy => a.syy,
        AggFunc::RegrSxy => a.sxy,
        AggFunc::RegrAvgX => a.sx / a.n,
        AggFunc::RegrAvgY => a.sy / a.n,
        AggFunc::CovarPop => a.sxy / a.n,
        AggFunc::CovarSamp => {
            if a.n < 2.0 {
                return Bson::Null;
            }
            a.sxy / (a.n - 1.0)
        }
        AggFunc::Corr => {
            if a.sxx == 0.0 || a.syy == 0.0 {
                return Bson::Null;
            }
            a.sxy / (a.sxx * a.syy).sqrt()
        }
        AggFunc::RegrR2 => {
            if a.sxx == 0.0 {
                return Bson::Null;
            }
            if a.syy == 0.0 {
                return float_bson(1.0);
            }
            (a.sxy * a.sxy) / (a.sxx * a.syy)
        }
        AggFunc::RegrSlope => {
            if a.sxx == 0.0 {
                return Bson::Null;
            }
            a.sxy / a.sxx
        }
        AggFunc::RegrIntercept => {
            if a.sxx == 0.0 {
                return Bson::Null;
            }
            (a.sy - a.sx * a.sxy / a.sxx) / a.n
        }
        _ => return Bson::Null,
    };
    float_bson(v)
}

/// A SQL value of type `ty` as a JSON value.
pub(crate) fn to_json(v: &Bson, ty: &str) -> Json {
    secantus_pgplan::jsonfn::to_json_value(v, ty)
}

/// A value's text inside a `json` aggregate.
fn json_text(v: &Bson, ty: &str) -> String {
    secantus_pgplan::jsonfn::datum_json_text(v, ty)
}

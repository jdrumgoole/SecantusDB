//! `$densify` — Rust port of `secantus.aggregate._stage_densify` (numeric path
//! only). Fills gaps between consecutive values of a numeric field, per
//! partition, at every multiple of `step` strictly between the bounds.
//!
//! Scope: the **numeric** densify (no `range.unit`). Date densify — fixed-
//! duration (`timedelta`) and variable-length (`relativedelta` month/quarter/
//! year) units — defers to pure Python (sub-millisecond `timedelta` steps and
//! calendar arithmetic are a fidelity risk). Anything non-numeric (a non-number
//! field value, non-numeric bounds, a partition key we can't canonicalise) also
//! defers, as does an explicit `bounds` range that would emit > 1M fillers
//! (Python raises there — we let it).
//!
//! The arithmetic mirrors Python exactly: the cursor is an int while `lo`/`step`
//! are ints (so `int + int` stays int) and widens to f64 once a float enters,
//! and `_densify_canon` collapses an integer-valued float filler back to an int.
//! Partition grouping and the `existing_values` membership reproduce Python's
//! dict / set semantics (`1 == 1.0 == True` collide) via the shared `group::GKey`
//! and `numeric::NumVal`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use bson::{Bson, Document};

use crate::group::{gkey, GKey};
use crate::numeric::{self, int_to_bson, NumVal};
use crate::paths;

type R<T> = Result<T, ()>;

/// Defensive cap so a pathological "full"-bounds range can't OOM / hang the
/// extension; we defer to Python rather than emit more than this.
const MAX_FILLERS: usize = 1_000_000;

#[derive(Clone, Copy)]
enum Num {
    Int(i128),
    Float(f64),
}

/// Numeric view of a BSON value (bool as 0/1, matching Python's `int`
/// hierarchy). `None` for non-numbers and NaN (which defers).
fn num_of(v: &Bson) -> Option<Num> {
    match v {
        Bson::Int32(n) => Some(Num::Int(*n as i128)),
        Bson::Int64(n) => Some(Num::Int(*n as i128)),
        Bson::Boolean(b) => Some(Num::Int(i128::from(*b))),
        Bson::Double(d) if !d.is_nan() => Some(Num::Float(*d)),
        _ => None,
    }
}

/// Python `a + b` preserving the int-vs-float distinction.
fn add(a: Num, b: Num) -> Num {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Num::Int(x + y),
        (Num::Int(x), Num::Float(y)) => Num::Float(x as f64 + y),
        (Num::Float(x), Num::Int(y)) => Num::Float(x + y as f64),
        (Num::Float(x), Num::Float(y)) => Num::Float(x + y),
    }
}

fn numval(n: Num) -> NumVal {
    match n {
        Num::Int(i) => numeric::from_int(i),
        Num::Float(f) => numeric::from_f64(f),
    }
}

fn to_f64(n: Num) -> f64 {
    match n {
        Num::Int(i) => i as f64,
        Num::Float(f) => f,
    }
}

fn cmp(a: Num, b: Num) -> Option<Ordering> {
    numeric::cmp(&numval(a), &numval(b))
}

/// `_densify_canon` + BSON encoding: an integer-valued float collapses to an
/// int (`2.0 -> 2`); width chosen by magnitude (int32/int64, else defer).
fn canon_to_bson(n: Num) -> R<Bson> {
    let n = match n {
        Num::Float(f) if f.is_finite() && f.fract() == 0.0 => Num::Int(f as i128),
        other => other,
    };
    match n {
        Num::Int(i) => int_to_bson(i).ok_or(()),
        Num::Float(f) => Ok(Bson::Double(f)),
    }
}

pub fn densify_stage(spec: &Bson, docs: &[Document]) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(());
    };
    let Some(Bson::String(field)) = s.get("field") else {
        return Err(());
    };
    let Some(Bson::Document(range_spec)) = s.get("range") else {
        return Err(());
    };
    // `unit` present (and not null) -> date densify -> defer.
    if range_spec
        .get("unit")
        .is_some_and(|u| !matches!(u, Bson::Null))
    {
        return Err(());
    }
    let Some(step) = range_spec.get("step").and_then(num_of) else {
        return Err(());
    };
    if cmp(step, Num::Int(0)) != Some(Ordering::Greater) {
        return Err(()); // step must be > 0 (Python raises otherwise)
    }

    // bounds: a numeric [lo, hi] pair, else "full"/"partition"/absent.
    let bounds_pair: Option<(Num, Num)> = match range_spec.get("bounds") {
        Some(Bson::Array(a)) if a.len() == 2 => match (num_of(&a[0]), num_of(&a[1])) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => return Err(()), // non-numeric explicit bounds -> Python compare raises
        },
        _ => None,
    };

    // 1M filler cap on explicit numeric bounds (Python raises -> defer).
    if let Some((lo, hi)) = bounds_pair {
        let stepf = to_f64(step);
        if stepf != 0.0 && (to_f64(hi) - to_f64(lo)) / stepf > MAX_FILLERS as f64 {
            return Err(());
        }
    }

    let partition_fields: Vec<&str> = match s.get("partitionByFields") {
        None | Some(Bson::Null) => Vec::new(),
        Some(Bson::Array(a)) => {
            let mut v = Vec::with_capacity(a.len());
            for x in a {
                match x {
                    Bson::String(f) => v.push(f.as_str()),
                    _ => return Err(()),
                }
            }
            v
        }
        Some(_) => return Err(()),
    };

    // Partition the docs (insertion-ordered), keyed like Python's dict.
    let mut order: Vec<Vec<GKey>> = Vec::new();
    let mut index: HashMap<Vec<GKey>, usize> = HashMap::new();
    let mut groups: Vec<Vec<&Document>> = Vec::new();
    for d in docs {
        let key = partition_key(d, &partition_fields)?;
        let idx = match index.get(&key) {
            Some(i) => *i,
            None => {
                let i = groups.len();
                index.insert(key.clone(), i);
                order.push(key);
                groups.push(Vec::new());
                i
            }
        };
        groups[idx].push(d);
    }

    // No input docs but explicit bounds: fill the whole range, no partition keys.
    if docs.is_empty() {
        return match bounds_pair {
            Some((lo, hi)) => fill_range(field, lo, hi, step, &Document::new()),
            None => Ok(Vec::new()),
        };
    }

    let mut out: Vec<Document> = Vec::new();
    for key in &order {
        let part = &groups[index[key]];
        // sorted(partition, key=get_path(field)) — numeric, stable. Any
        // missing / non-numeric / NaN field value defers (Python sort raises).
        let mut keyed: Vec<(Num, &Document)> = Vec::with_capacity(part.len());
        for d in part {
            let Some(num) = paths::get_path(d, field).and_then(num_of) else {
                return Err(());
            };
            keyed.push((num, d));
        }
        keyed.sort_by(|(a, _), (b, _)| cmp(*a, *b).unwrap_or(Ordering::Equal));

        let mut carry = Document::new();
        let first = keyed[0].1;
        for f in &partition_fields {
            let v = paths::get_path(first, f).cloned().unwrap_or(Bson::Null);
            carry.insert((*f).to_string(), v);
        }

        let (lo, hi) = match bounds_pair {
            Some((l, h)) => (l, h),
            None => (keyed[0].0, keyed[keyed.len() - 1].0),
        };
        out.extend(densify_partition(field, &keyed, lo, hi, step, &carry)?);
    }
    Ok(out)
}

fn partition_key(doc: &Document, fields: &[&str]) -> R<Vec<GKey>> {
    let mut key = Vec::with_capacity(fields.len());
    for f in fields {
        let v = paths::get_path(doc, f).cloned().unwrap_or(Bson::Null);
        key.push(gkey(&v).map_err(|_| ())?);
    }
    Ok(key)
}

fn fill_range(field: &str, lo: Num, hi: Num, step: Num, carry: &Document) -> R<Vec<Document>> {
    let mut out = Vec::new();
    let mut cursor = lo;
    while cmp(cursor, hi) == Some(Ordering::Less) {
        if out.len() >= MAX_FILLERS {
            return Err(());
        }
        let mut filler = carry.clone();
        filler.insert(field.to_string(), canon_to_bson(cursor)?);
        out.push(filler);
        cursor = add(cursor, step);
    }
    Ok(out)
}

fn densify_partition(
    field: &str,
    keyed: &[(Num, &Document)],
    lo: Num,
    hi: Num,
    step: Num,
    carry: &Document,
) -> R<Vec<Document>> {
    let existing: HashSet<NumVal> = keyed.iter().map(|(n, _)| numval(*n)).collect();
    let mut out: Vec<Document> = Vec::new();
    let mut cursor = lo;
    let mut iter = keyed.iter();
    let mut next = iter.next();
    while cmp(cursor, hi) == Some(Ordering::Less) {
        if out.len() >= MAX_FILLERS {
            return Err(());
        }
        if let Some((nval, ndoc)) = next {
            if numeric::eq(&numval(*nval), &numval(cursor)) {
                out.push((*ndoc).clone());
                next = iter.next();
                cursor = add(cursor, step);
                continue;
            }
        }
        if !existing.contains(&numval(cursor)) {
            let mut filler = carry.clone();
            filler.insert(field.to_string(), canon_to_bson(cursor)?);
            out.push(filler);
        }
        cursor = add(cursor, step);
    }
    // Originals at or beyond `hi` must still appear.
    while let Some((_, ndoc)) = next {
        out.push((*ndoc).clone());
        next = iter.next();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn run(spec: bson::Bson, docs: Vec<Document>) -> Vec<Document> {
        densify_stage(&spec, &docs).expect("should not defer")
    }

    #[test]
    fn numeric_full_bounds() {
        let out = run(
            bson::bson!({"field": "v", "range": {"step": 2, "bounds": "full"}}),
            vec![doc! {"v": 0i32}, doc! {"v": 6i32}],
        );
        // fillers at 2, 4 between 0 and 6; originals kept.
        let vals: Vec<i32> = out.iter().map(|d| d.get_i32("v").unwrap()).collect();
        assert_eq!(vals, vec![0, 2, 4, 6]);
    }

    #[test]
    fn explicit_bounds_and_canon() {
        let out = run(
            bson::bson!({"field": "v", "range": {"step": 1, "bounds": [0, 3]}}),
            vec![doc! {"v": 1.0f64}],
        );
        // 0 (filler), 1.0 (original at cursor==1.0), 2 (filler); 3 excluded (< hi).
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].get("v"), Some(&Bson::Int32(0)));
        assert_eq!(out[1].get("v"), Some(&Bson::Double(1.0))); // original untouched
        assert_eq!(out[2].get("v"), Some(&Bson::Int32(2)));
    }

    #[test]
    fn partitioned() {
        let out = run(
            bson::bson!({
                "field": "v",
                "partitionByFields": ["g"],
                "range": {"step": 1, "bounds": "full"},
            }),
            vec![
                doc! {"g": "a", "v": 0i32},
                doc! {"g": "a", "v": 2i32},
                doc! {"g": "b", "v": 5i32},
            ],
        );
        // partition a: 0, filler 1, 2; partition b: just 5.
        assert_eq!(out.len(), 4);
        assert_eq!(out[1].get("g"), Some(&Bson::String("a".into()))); // filler carries g
        assert_eq!(out[1].get("v"), Some(&Bson::Int32(1)));
    }

    #[test]
    fn date_unit_defers() {
        assert!(densify_stage(
            &bson::bson!({"field": "t", "range": {"step": 1, "unit": "day", "bounds": "full"}}),
            &[doc! {"t": "x"}]
        )
        .is_err());
    }

    #[test]
    fn huge_explicit_bounds_defers() {
        assert!(densify_stage(
            &bson::bson!({"field": "v", "range": {"step": 1, "bounds": [0i64, 5_000_000i64]}}),
            &[doc! {"v": 0i32}]
        )
        .is_err());
    }
}

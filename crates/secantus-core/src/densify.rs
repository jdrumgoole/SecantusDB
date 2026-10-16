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

use crate::fallback::Fallback;
use crate::group::{gkey, GKey};
use crate::numeric::{self, int_to_bson, NumVal};
use crate::paths;

/// Carries `Fallback` rather than `()` so an error the expression engine NAMED
/// survives to the client instead of being flattened into the generic
/// "not supported by the Rust server". `Fallback::Defer` is the same
/// "cannot reproduce this" signal the unit `()` used to be.
type R<T> = Result<T, Fallback>;

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
        Num::Int(i) => int_to_bson(i).ok_or(Fallback::Defer),
        Num::Float(f) => Ok(Bson::Double(f)),
    }
}

pub fn densify_stage(spec: &Bson, docs: &[Document]) -> R<Vec<Document>> {
    let Bson::Document(s) = spec else {
        return Err(Fallback::Defer);
    };
    let Some(Bson::String(field)) = s.get("field") else {
        return Err(Fallback::Defer);
    };
    let Some(Bson::Document(range_spec)) = s.get("range") else {
        return Err(Fallback::Defer);
    };
    // `unit` present (and not null) -> date densify -> defer.
    if range_spec
        .get("unit")
        .is_some_and(|u| !matches!(u, Bson::Null))
    {
        return Err(Fallback::Defer);
    }
    // A bool step is "wrong type" (Python raises 14) — don't let num_of coerce it.
    if matches!(range_spec.get("step"), Some(Bson::Boolean(_))) {
        return Err(Fallback::Defer);
    }
    let Some(step) = range_spec.get("step").and_then(num_of) else {
        return Err(Fallback::Defer);
    };
    if cmp(step, Num::Int(0)) != Some(Ordering::Greater) {
        return Err(Fallback::Defer); // step must be > 0 (Python raises 5733401)
    }

    // bounds: the string "full"/"partition", or a strictly-ascending two-element
    // numeric array; every other shape defers (Python raises 5946802/5733403/5733402).
    let bounds_pair: Option<(Num, Num)> = match range_spec.get("bounds") {
        None | Some(Bson::Null) => None,
        Some(Bson::String(s)) if s == "full" || s == "partition" => None,
        Some(Bson::Array(a)) if a.len() == 2 => {
            if a.iter().any(|b| matches!(b, Bson::Boolean(_))) {
                return Err(Fallback::Defer); // bool bounds element -> Python 5733402
            }
            match (num_of(&a[0]), num_of(&a[1])) {
                (Some(lo), Some(hi)) => {
                    if cmp(lo, hi) != Some(Ordering::Less) {
                        return Err(Fallback::Defer); // not strictly ascending -> Python 5733402
                    }
                    Some((lo, hi))
                }
                _ => return Err(Fallback::Defer), // non-numeric (date/other) bounds -> Python
            }
        }
        _ => return Err(Fallback::Defer), // bad string / wrong-length array / other type -> Python
    };

    // 1M filler cap on explicit numeric bounds (Python raises -> defer).
    if let Some((lo, hi)) = bounds_pair {
        let stepf = to_f64(step);
        if stepf != 0.0 && (to_f64(hi) - to_f64(lo)) / stepf > MAX_FILLERS as f64 {
            return Err(Fallback::Defer);
        }
    }

    let partition_fields: Vec<&str> = match s.get("partitionByFields") {
        None | Some(Bson::Null) => Vec::new(),
        Some(Bson::Array(a)) => {
            let mut v = Vec::with_capacity(a.len());
            for x in a {
                match x {
                    Bson::String(f) => v.push(f.as_str()),
                    _ => return Err(Fallback::Defer),
                }
            }
            v
        }
        Some(_) => return Err(Fallback::Defer),
    };

    if !partition_fields.is_empty()
        && matches!(range_spec.get("bounds"), Some(Bson::String(b)) if b == "full")
    {
        return full_partitioned(field, docs, step, &partition_fields);
    }

    // Partition the docs (insertion-ordered), keyed like Python's dict.
    let mut order: Vec<Vec<Option<GKey>>> = Vec::new();
    let mut index: HashMap<Vec<Option<GKey>>, usize> = HashMap::new();
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
        // A doc whose densify field is missing or null does NOT participate:
        // mongod emits it unchanged and it contributes neither a bound nor a
        // step. Null sorts before every number, so those lead the output.
        //
        // This used to defer the whole stage on any such doc, with a comment
        // saying "Python sort raises" — which it did, with a bare TypeError
        // that escaped as "internal server error". Both sides are fixed
        // together; a non-numeric, non-date value still defers so the Python
        // engine can raise mongod's typed 5733201.
        let mut keyed: Vec<(Num, &Document)> = Vec::with_capacity(part.len());
        for d in part {
            match paths::get_path(d, field) {
                None | Some(Bson::Null) => out.push((*d).clone()),
                Some(v) => match num_of(v) {
                    Some(num) => keyed.push((num, d)),
                    None => return Err(Fallback::Defer), // non-numeric/date -> Python raises 5733201
                },
            }
        }
        if keyed.is_empty() {
            continue;
        }
        keyed.sort_by(|(a, _), (b, _)| cmp(*a, *b).unwrap_or(Ordering::Equal));

        // A filler past the last document carries that document's partition
        // values; one before a document carries that document's (see
        // `densify_partition`).
        let carry = carry_of(keyed[keyed.len() - 1].1, &partition_fields)?;

        let (lo, hi) = match bounds_pair {
            Some((l, h)) => (l, h),
            None => (keyed[0].0, keyed[keyed.len() - 1].0),
        };
        out.extend(densify_partition(
            field,
            &keyed,
            lo,
            hi,
            step,
            &carry,
            &partition_fields,
        )?);
    }
    Ok(out)
}

/// `bounds: "full"` with `partitionByFields`: every partition is filled
/// across the whole collection's range. A transcription of mongod 8.2's
/// `DocumentSourceInternalDensify` for that case
/// (`src/mongo/db/pipeline/document_source_densify.cpp`): the documents are
/// read in order of the field (stably; a missing or null field passes
/// through first), the first value read is the global minimum and the last
/// the maximum, and each partition remembers the last value on its step
/// grid. A partition's first document between the minimum and one step
/// above it records the step BELOW the minimum, so the partition is filled
/// from the minimum again at the end -- repeating that document's value
/// when it is on the grid, as mongod does. The partitions are finished in
/// the order they were first seen (mongod's order is a hash table's, and
/// varies between runs).
fn full_partitioned(
    field: &str,
    docs: &[Document],
    step: Num,
    partition_fields: &[&str],
) -> R<Vec<Document>> {
    let mut sorted: Vec<(Option<Num>, &Document)> = Vec::with_capacity(docs.len());
    for d in docs {
        let v = match paths::get_path(d, field) {
            None | Some(Bson::Null) => None,
            Some(v) => Some(num_of(v).ok_or(Fallback::Defer)?),
        };
        sorted.push((v, d));
    }
    sorted.sort_by(|(a, _), (b, _)| match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => cmp(*x, *y).unwrap_or(Ordering::Equal),
    });
    let less = |a: Num, b: Num| cmp(a, b) == Some(Ordering::Less);
    let sub = |a: Num| match (a, step) {
        (Num::Int(x), Num::Int(y)) => Num::Int(x - y),
        (x, y) => Num::Float(to_f64(x) - to_f64(y)),
    };
    let filler = |carry: &Document, v: Num| -> R<Document> {
        let mut d = carry.clone();
        d.insert(field.to_string(), canon_to_bson(v)?);
        Ok(d)
    };
    // Per partition, in the order first seen: its key, the partition values
    // a filler carries, and the last value on its grid.
    let mut table: Vec<(Vec<Option<GKey>>, Document, Num)> = Vec::new();
    let mut out: Vec<Document> = Vec::new();
    let mut global: Option<(Num, Num)> = None;
    for (v, d) in sorted {
        let Some(x) = v else {
            out.push(d.clone());
            continue;
        };
        let (min, max) = match global {
            Some((min, _)) => (min, x),
            None => (x, x),
        };
        global = Some((min, max));
        let key = partition_key(d, partition_fields)?;
        let carry = carry_of(d, partition_fields)?;
        // `handleNeedGen`: fill from the step after `from` up to (not
        // including) `x`, then the document. Returns the partition's grid
        // value: the last one generated, or `x` when it is on the grid.
        let need_gen = |from: Num, mut grid: Num, out: &mut Vec<Document>| -> R<Num> {
            let mut next = add(from, step);
            let mut generated = 0usize;
            while less(next, x) {
                generated += 1;
                if generated > MAX_FILLERS {
                    return Err(Fallback::Defer);
                }
                out.push(filler(&carry, next)?);
                grid = next;
                next = add(next, step);
            }
            if cmp(next, x) == Some(Ordering::Equal) {
                grid = x;
            }
            out.push(d.clone());
            Ok(grid)
        };
        match table.iter().position(|(k, _, _)| *k == key) {
            Some(at) => {
                let l = table[at].2;
                if less(x, l) {
                    out.push(d.clone());
                } else {
                    table[at].2 = need_gen(l, l, &mut out)?;
                }
            }
            None => {
                // `checkFirstDocAgainstRangeStart`, the full case.
                let grid = if less(min, x) && less(add(min, step), x) {
                    need_gen(sub(min), sub(min), &mut out)?
                } else if less(min, x) {
                    out.push(d.clone());
                    sub(min)
                } else {
                    out.push(d.clone());
                    x
                };
                table.push((key, carry.clone(), grid));
            }
        }
    }
    // `finishDensifyingPartitionedInputHelper`: each partition up to the
    // maximum, inclusive.
    if let Some((min, max)) = global {
        for (_, carry, last) in &table {
            let mut v = add(*last, step);
            if less(v, min) {
                v = min;
            }
            let mut generated = 0usize;
            while !less(max, v) {
                generated += 1;
                if generated > MAX_FILLERS {
                    return Err(Fallback::Defer);
                }
                out.push(filler(carry, v)?);
                v = add(v, step);
            }
        }
    }
    Ok(out)
}

/// The partition fields of `doc`, as a filler carries them. mongod's
/// partition key is an object expression built from the paths
/// (`{a: {b: "$a.b"}}` for `a.b`), so a dotted field is nested, a missing
/// one is left out, and the objects around it stay (`{a: {}}`). Measured
/// 8.2.11, 2026-10-16; this wrote a literal `"a.b"` key, and a `null` for a
/// missing field.
fn carry_of(doc: &Document, fields: &[&str]) -> R<Document> {
    let mut carry = Document::new();
    for f in fields {
        let parts: Vec<&str> = f.split('.').collect();
        let mut at = &mut carry;
        for part in &parts[..parts.len() - 1] {
            if !matches!(at.get(*part), Some(Bson::Document(_))) {
                at.insert((*part).to_string(), Document::new());
            }
            let Some(Bson::Document(inner)) = at.get_mut(*part) else {
                return Err(Fallback::Defer);
            };
            at = inner;
        }
        if let Some(v) = paths::get_path(doc, f) {
            at.insert(parts[parts.len() - 1].to_string(), v.clone());
        }
    }
    Ok(carry)
}

/// The partition a document is in. A missing field and a null one are two
/// partitions, as the documents mongod keys them by differ.
fn partition_key(doc: &Document, fields: &[&str]) -> R<Vec<Option<GKey>>> {
    let mut key = Vec::with_capacity(fields.len());
    for f in fields {
        key.push(match paths::get_path(doc, f) {
            Some(v) => Some(gkey(v).map_err(|_| Fallback::Defer)?),
            None => None,
        });
    }
    Ok(key)
}

fn fill_range(field: &str, lo: Num, hi: Num, step: Num, carry: &Document) -> R<Vec<Document>> {
    let mut out = Vec::new();
    let mut cursor = lo;
    while cmp(cursor, hi) == Some(Ordering::Less) {
        if out.len() >= MAX_FILLERS {
            return Err(Fallback::Defer);
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
    partition_fields: &[&str],
) -> R<Vec<Document>> {
    let existing: HashSet<NumVal> = keyed.iter().map(|(n, _)| numval(*n)).collect();
    let mut out: Vec<Document> = Vec::new();
    let mut cursor = lo;
    let mut iter = keyed.iter();
    let mut next = iter.next();
    while cmp(cursor, hi) == Some(Ordering::Less) {
        if out.len() >= MAX_FILLERS {
            return Err(Fallback::Defer);
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
            // The partition values are those of the document the filler
            // leads up to. They can differ within one partition under a
            // collation: between `"a"` and `"A"` mongod writes `"A"`
            // (measured 8.2.11, 2026-10-10).
            let mut filler = match next {
                Some((_, upcoming)) => carry_of(upcoming, partition_fields)?,
                None => carry.clone(),
            };
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
                doc! {"_id": 1, "g": "a", "v": 0i32},
                doc! {"_id": 2, "g": "a", "v": 2i32},
                doc! {"_id": 3, "g": "b", "v": 5i32},
            ],
        );
        // Every partition is filled across the collection's range, 0..=5,
        // in mongod 8.2.11's order: `a` up to its 2, `b` from 0 up to its 5,
        // then `a` on to 5. A filler carries its partition's `g`.
        let got: Vec<(String, i32, bool)> = out
            .iter()
            .map(|d| {
                (
                    d.get_str("g").unwrap().to_string(),
                    d.get_i32("v").unwrap(),
                    d.contains_key("_id"),
                )
            })
            .collect();
        let want: Vec<(String, i32, bool)> = [
            ("a", 0, true),
            ("a", 1, false),
            ("a", 2, true),
            ("b", 0, false),
            ("b", 1, false),
            ("b", 2, false),
            ("b", 3, false),
            ("b", 4, false),
            ("b", 5, true),
            ("a", 3, false),
            ("a", 4, false),
            ("a", 5, false),
        ]
        .iter()
        .map(|(g, v, s)| (g.to_string(), *v, *s))
        .collect();
        assert_eq!(got, want);
    }

    /// mongod 8.2.11: a missing partition field and a null one are two
    /// partitions, and a filler leaves a missing field out.
    #[test]
    fn missing_and_null_partition_apart() {
        let out = run(
            bson::bson!({
                "field": "v",
                "partitionByFields": ["g"],
                "range": {"step": 1, "bounds": "full"},
            }),
            vec![
                doc! {"_id": 0, "v": 0},
                doc! {"_id": 1, "v": 3, "g": Bson::Null},
                doc! {"_id": 2, "v": 1, "g": Bson::Null},
            ],
        );
        assert_eq!(
            out,
            vec![
                doc! {"_id": 0, "v": 0},
                doc! {"_id": 2, "v": 1, "g": Bson::Null},
                doc! {"g": Bson::Null, "v": 0},
                doc! {"g": Bson::Null, "v": 1},
                doc! {"g": Bson::Null, "v": 2},
                doc! {"_id": 1, "v": 3, "g": Bson::Null},
                doc! {"v": 1},
                doc! {"v": 2},
                doc! {"v": 3},
            ]
        );
    }

    /// mongod 8.2.11: a dotted partition field is nested in a filler.
    #[test]
    fn a_dotted_partition_field_is_nested() {
        for bounds in ["full", "partition"] {
            let out = run(
                bson::bson!({
                    "field": "v",
                    "partitionByFields": ["a.b"],
                    "range": {"step": 1, "bounds": bounds},
                }),
                vec![
                    doc! {"_id": 0, "v": 0, "a": {"b": 1}},
                    doc! {"_id": 1, "v": 2, "a": {"b": 1}},
                ],
            );
            assert_eq!(out[1], doc! {"a": {"b": 1}, "v": 1}, "{bounds}");
        }
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

//! A correlated subquery's table scan, partitioned once per statement.
//!
//! A correlated subquery whose shape the semi-join hash path cannot take
//! (`secantus_pgplan::semijoin_hash` -- a WHERE with a nested correlated
//! `EXISTS`, say) runs once per outer row. When its filter pins a column
//! with no index to a value (`t.x = $1`), every one of those runs was a
//! full scan of the table: 2,000 outer rows over a 2,000-row table read
//! 4,000,000 rows, 1.2 s where PostgreSQL hashes the table once (0.13 s).
//!
//! Here the table is read ONCE for the statement and its rows grouped by
//! that column's value; each later run takes only its value's rows, in the
//! table's scan order, and re-checks them against the whole filter exactly
//! as the storage scan would. The grouping is only a narrowing: a value it
//! cannot normalise (an array, a numeric, a document) makes the scan go on
//! as before.
//!
//! The entries live in `secantus_pgplan::with_scan_cache`, which exists
//! only while a correlated subquery of a statement runs and is dropped with
//! the statement -- the same lifetime, and the same rule for user-code
//! bodies, as the semi-join cache's.

use std::collections::HashMap;
use std::rc::Rc;

use bson::{Bson, Document};

/// Runs of the same scan before it is worth reading the table whole: a
/// statement that looks up one or two values never pays for the build.
const BUILD_AFTER: u32 = 4;

/// A value's bucket: equal values (as MQL equality sees them) share one.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum PKey {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    Date(i64),
}

/// The bucket of `v`, or `None` for a value grouped by nothing here.
fn pkey(v: &Bson) -> Option<PKey> {
    let float = |d: f64| -> PKey {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        if d.is_finite() && d.fract() == 0.0 && d.abs() < 9.0e18 {
            PKey::Int(d as i64)
        } else if d.is_nan() {
            PKey::Float(f64::NAN.to_bits())
        } else {
            PKey::Float(d.to_bits())
        }
    };
    Some(match v {
        Bson::Null => PKey::Null,
        Bson::Boolean(b) => PKey::Bool(*b),
        Bson::Int32(i) => PKey::Int(i64::from(*i)),
        Bson::Int64(i) => PKey::Int(*i),
        Bson::Double(d) => float(*d),
        Bson::String(s) => PKey::Str(s.clone()),
        Bson::DateTime(d) => PKey::Date(d.timestamp_millis()),
        _ => return None,
    })
}

enum State {
    /// Seen this many times; not built yet.
    Counting(u32),
    /// Not worth it here: an index serves the filter, or a row's value has
    /// no bucket.
    No,
    Built(Rc<Part>),
}

struct Part {
    buckets: HashMap<PKey, Vec<usize>>,
    rows: Vec<Vec<u8>>,
}

/// The `field = value` a filter pins, when it pins one plainly: a top-level
/// `{field: scalar}` / `{field: {$eq: scalar}}`, or one inside a top-level
/// `$and`. The stored key (`_id`) and dotted paths are left to the storage,
/// which has its own lookups for them.
fn equality_of(filter: &Document) -> Option<(&str, &Bson)> {
    for (k, v) in filter {
        if k == "$and" {
            if let Bson::Array(arms) = v {
                for arm in arms {
                    if let Bson::Document(d) = arm {
                        if let Some(found) = equality_of(d) {
                            return Some(found);
                        }
                    }
                }
            }
            continue;
        }
        if k.starts_with('$') || k.starts_with("_id") || k.contains('.') {
            continue;
        }
        let value = match v {
            Bson::Document(d) if d.len() == 1 => match d.get("$eq") {
                Some(x) => x,
                None => continue,
            },
            Bson::Document(_) | Bson::Array(_) => continue,
            other => other,
        };
        if pkey(value).is_some() && !matches!(value, Bson::Null) {
            return Some((k.as_str(), value));
        }
    }
    None
}

/// The rows of `table` matching `filter`, from the statement's partition of
/// the table -- or `None` when the storage should scan as it always has.
/// `scan_all` reads every row in scan order; `indexed` says whether the
/// storage would serve `filter` from an index.
pub(crate) fn rows_matching(
    db: &str,
    table: &str,
    filter: &Document,
    scan_all: impl FnOnce() -> Option<Vec<Vec<u8>>>,
    indexed: impl FnOnce() -> bool,
) -> Option<Vec<Vec<u8>>> {
    let (field, value) = equality_of(filter)?;
    let want = pkey(value)?;
    let key = format!("scan-partition\u{1f}{db}\u{1f}{table}\u{1f}{field}");
    // Count this run; build on the run that crosses the threshold.
    let part = secantus_pgplan::with_scan_cache(|cache| {
        let state = cache
            .entry(key.clone())
            .or_insert_with(|| Box::new(State::Counting(0)));
        let state = state.downcast_mut::<State>()?;
        match state {
            State::No => None,
            State::Built(p) => Some(Some(Rc::clone(p))),
            State::Counting(n) => {
                *n += 1;
                (*n >= BUILD_AFTER).then_some(None)
            }
        }
    })??;
    let part = match part {
        Some(p) => p,
        None => {
            let built = if indexed() {
                None
            } else {
                scan_all().and_then(|rows| build(rows, field))
            };
            let state = match &built {
                Some(p) => State::Built(Rc::clone(p)),
                None => State::No,
            };
            secantus_pgplan::with_scan_cache(|cache| {
                cache.insert(key, Box::new(state));
            });
            built?
        }
    };
    let empty = Document::new();
    let mut out = Vec::new();
    for &i in part.buckets.get(&want).map_or(&[][..], Vec::as_slice) {
        let raw = &part.rows[i];
        let doc: Document = bson::from_slice(raw).ok()?;
        // Any error is the storage scan's to raise, as it always did.
        if secantus_core::query::matches(&doc, filter, &empty, None).ok()? {
            out.push(raw.clone());
        }
    }
    Some(out)
}

/// The most a partition holds: past it the table is scanned per run, as
/// before, rather than kept in memory for the statement.
const MAX_BYTES: usize = 256 << 20;

/// `rows` grouped by `field`, or `None` when a value has no bucket (or the
/// table is too big to keep).
fn build(rows: Vec<Vec<u8>>, field: &str) -> Option<Rc<Part>> {
    if rows.iter().map(Vec::len).sum::<usize>() > MAX_BYTES {
        return None;
    }
    let mut buckets: HashMap<PKey, Vec<usize>> = HashMap::new();
    for (i, raw) in rows.iter().enumerate() {
        let doc = bson::RawDocument::from_bytes(raw).ok()?;
        let key = match doc.get(field).ok()? {
            None => PKey::Null,
            Some(v) => pkey(&Bson::try_from(v.to_raw_bson()).ok()?)?,
        };
        buckets.entry(key).or_default().push(i);
    }
    Some(Rc::new(Part { buckets, rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_numbers_share_a_bucket_and_other_values_do_not() {
        assert_eq!(pkey(&Bson::Int32(5)), pkey(&Bson::Int64(5)));
        assert_eq!(pkey(&Bson::Int32(5)), pkey(&Bson::Double(5.0)));
        assert_eq!(pkey(&Bson::Double(0.0)), pkey(&Bson::Double(-0.0)));
        assert_ne!(pkey(&Bson::Double(5.5)), pkey(&Bson::Int32(5)));
        assert_eq!(
            pkey(&Bson::Double(f64::NAN)),
            pkey(&Bson::Double(-f64::NAN))
        );
        assert_eq!(pkey(&Bson::Array(vec![])), None);
    }

    #[test]
    fn only_a_plain_equality_pins_a_field() {
        let f = bson::doc! { "x": 5, "y": { "$gt": 1 } };
        assert_eq!(equality_of(&f), Some(("x", &Bson::Int32(5))));
        let f = bson::doc! { "$and": [ { "y": { "$gt": 1 } }, { "x": { "$eq": "a" } } ] };
        assert_eq!(equality_of(&f).map(|(k, _)| k), Some("x"));
        assert_eq!(equality_of(&bson::doc! { "_id": 1 }), None);
        assert_eq!(equality_of(&bson::doc! { "x": [1] }), None);
        assert_eq!(equality_of(&bson::doc! { "x": { "$in": [1] } }), None);
        assert_eq!(equality_of(&bson::doc! { "x": Bson::Null }), None);
    }
}

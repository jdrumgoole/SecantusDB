//! BSON sort ordering — Rust port of `secantus.storage._bson_lt` /
//! `_bson_type_rank`, the cross-type comparator the aggregation `$sort` stage
//! (and `sort_docs`) uses.
//!
//! Exposed as a total `cmp(a, b) -> Ordering` that mirrors how Python's
//! `sorted()` drives `_SortKey.__lt__`: `Less` when `_bson_lt(a, b)`, `Greater`
//! when `_bson_lt(b, a)`, else `Equal` (stable — preserves input order). To stay
//! strictly faithful the caller first runs `is_sortable` over every sort-key
//! value; anything that would hit Python's `Decimal128` widening, a `TypeError`
//! → type-name fallback, or an exotic BSON type defers the whole stage to
//! Python, so `cmp` itself never has to represent "can't compare".

use std::cmp::Ordering;

use bson::Bson;

use crate::numeric;

/// MongoDB's cross-type sort rank (lower sorts first), matching
/// `_bson_type_rank`. Only the types we can faithfully compare get a rank;
/// `is_sortable` gates everything else out before `cmp` runs.
/// `Bson::Undefined` stands in for an empty array in a sort key — see
/// [`array_sort_value`]. pymongo never encodes `undefined`, so it cannot collide
/// with a stored value.
pub const EMPTY_ARRAY_SORT_MARKER: Bson = Bson::Undefined;

/// Ranks are spaced by 10 so the empty-array marker can sit *between* MinKey and
/// Null, which is where mongod puts it. Relative order is otherwise unchanged.
fn type_rank(v: &Bson) -> u8 {
    match v {
        Bson::MinKey => 10,
        Bson::Undefined => 15, // empty-array sort marker: above MinKey, below Null
        Bson::Null => 20,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => 30,
        Bson::String(_) => 40,
        Bson::Document(_) => 50,
        Bson::Array(_) => 60,
        Bson::Binary(_) => 70,
        Bson::ObjectId(_) => 80,
        Bson::Boolean(_) => 90,
        Bson::DateTime(_) => 100,
        Bson::Timestamp(_) => 110,
        Bson::RegularExpression(_) => 120,
        Bson::MaxKey => 130,
        _ => 50, // matches Python's `return 5` fallback (never reached: is_sortable bars these)
    }
}

/// The value mongod actually sorts an ARRAY-valued field by: its minimum element
/// ascending, its maximum descending.
///
/// Verified against mongod 6.0.16 — `[[1,100], [5,9], 6, [7]]` sorts ascending as
/// `[1,100] < [5,9] < 6 < [7]` (minima 1 < 5 < 6 < 7) and descending by maxima
/// 100 > 9 > 7 > 6. An empty array has no representative and sorts between MinKey
/// and Null.
///
/// Comparing whole arrays put every array after every scalar, and worse, it
/// disagreed with our own index path: a multikey index writes one entry per
/// element, so an index scan already produced mongod's ordering and the same query
/// returned a different order depending on whether an index existed.
///
/// Returns `None` when an element is not faithfully sortable, so the caller can
/// raise its own module's `Fallback`. Mirrors `ordering.py::_array_sort_value`.
pub fn array_sort_value(v: Bson, reverse: bool) -> Option<Bson> {
    let Bson::Array(items) = v else {
        return Some(v);
    };
    if items.is_empty() {
        return Some(EMPTY_ARRAY_SORT_MARKER);
    }
    let mut best: Option<Bson> = None;
    for item in items {
        if !is_sortable(&item) {
            return None;
        }
        best = Some(match best {
            None => item,
            Some(cur) => {
                let take = if reverse {
                    cmp(&item, &cur) == std::cmp::Ordering::Greater
                } else {
                    cmp(&item, &cur) == std::cmp::Ordering::Less
                };
                if take {
                    item
                } else {
                    cur
                }
            }
        });
    }
    Some(best.unwrap_or(Bson::Null))
}

/// True if every value in `v` is one we can sort *faithfully* — meaning Python
/// `==` agrees with `cmp(...) == Equal` for it.
///
/// This gate is what makes the multi-field comparator correct. `sort_docs`
/// wraps keys in `_SortKey` whose `__eq__` is Python `==` but whose `__lt__` is
/// rank-based `_bson_lt`; a tuple sort advances to the next field only when `==`
/// is True. For the types below the two relations coincide, so the comparator
/// is a consistent total preorder and a stable sort reproduces Python exactly.
/// The types where they *diverge* — `bool` (`False == 0`, `True == 1`, but a
/// different BSON rank), `NaN` (`nan != nan`), and the `==`-False-but-`<`-both-
/// False cases (Binary w/ subtype, Timestamp, Regex, Min/MaxKey) — make the
/// comparator non-transitive, so we defer the whole `$sort` to pure Python
/// rather than risk diverging from its Timsort. Decimal128 and exotic types
/// defer too.
pub fn is_sortable(v: &Bson) -> bool {
    match v {
        Bson::Null
        | Bson::Int32(_)
        | Bson::Int64(_)
        | Bson::String(_)
        | Bson::ObjectId(_)
        | Bson::DateTime(_) => true,
        Bson::Double(d) => !d.is_nan(),
        Bson::Document(d) => d.values().all(is_sortable),
        Bson::Array(a) => a.iter().all(is_sortable),
        // bool, NaN, Binary, Timestamp, Regex, Min/MaxKey, Decimal128, exotic.
        _ => false,
    }
}

/// Total BSON sort comparison. Assumes both operands passed `is_sortable`.
pub fn cmp(a: &Bson, b: &Bson) -> Ordering {
    let (ra, rb) = (type_rank(a), type_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        // Two nulls / two MinKeys / two MaxKeys: Python's native `<` is False
        // both ways -> equal (stable).
        (Bson::Null, _) | (Bson::MinKey, _) | (Bson::MaxKey, _) => Ordering::Equal,
        (Bson::String(x), Bson::String(y)) => x.cmp(y),
        (Bson::Boolean(x), Bson::Boolean(y)) => x.cmp(y),
        (Bson::DateTime(x), Bson::DateTime(y)) => x.timestamp_millis().cmp(&y.timestamp_millis()),
        (Bson::Timestamp(x), Bson::Timestamp(y)) => {
            (x.time, x.increment).cmp(&(y.time, y.increment))
        }
        (Bson::ObjectId(x), Bson::ObjectId(y)) => x.bytes().cmp(&y.bytes()),
        (Bson::Binary(x), Bson::Binary(y)) => x.bytes.cmp(&y.bytes), // subtype ignored (bytes `<`)
        // Two regexes: Python `<` raises TypeError -> type-name fallback ->
        // "Regex" == "Regex" -> not less -> equal.
        (Bson::RegularExpression(_), Bson::RegularExpression(_)) => Ordering::Equal,
        (Bson::Document(x), Bson::Document(y)) => doc_cmp(x, y),
        (Bson::Array(x), Bson::Array(y)) => array_cmp(x, y),
        // Rank 3: the unified numeric type. NaN is unordered -> Python's `<` is
        // False both ways -> Equal (stable).
        _ => {
            if let Some(r) = numeric::fast_cmp(a, b) {
                return r.unwrap_or(Ordering::Equal);
            }
            match (numeric::classify(a), numeric::classify(b)) {
                (Some(na), Some(nb)) => numeric::cmp(&na, &nb).unwrap_or(Ordering::Equal),
                _ => Ordering::Equal,
            }
        }
    }
}

/// Field-by-field document comparison in insertion order (matches `_bson_lt`'s
/// Mapping branch): first differing key compares as strings, else recurse into
/// values; finally the shorter document sorts first.
fn doc_cmp(a: &bson::Document, b: &bson::Document) -> Ordering {
    for ((ak, av), (bk, bv)) in a.iter().zip(b.iter()) {
        if ak != bk {
            return ak.cmp(bk);
        }
        let c = cmp(av, bv);
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

/// Lexicographic, element-by-element; shorter array sorts first on a tie.
fn array_cmp(a: &[Bson], b: &[Bson]) -> Ordering {
    for (av, bv) in a.iter().zip(b.iter()) {
        let c = cmp(av, bv);
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

/// `ordering._bson_lt`'s rank, extending [`type_rank`] with the ranks the
/// *decoded* Python values carry: pymongo hands the Python engine `str` for a
/// BSON Symbol and the str-subclass `Code` for JS code (rank 4), and `None`
/// for undefined (rank 2). A DBPointer decodes to an unranked object (default
/// rank 5) — [`bson_lt`] defers it rather than reproduce Python's
/// type-*name* tiebreak.
fn lt_rank(v: &Bson) -> u8 {
    match v {
        Bson::Symbol(_) | Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => 4,
        Bson::Undefined => 2,
        _ => type_rank(v),
    }
}

/// The text a rank-4 value compares by: Python sees plain `str` for String /
/// Symbol / JS code (`Code` is a str subclass; a with-scope Code compares by
/// its code string, scope ignored).
fn lt_text(v: &Bson) -> Option<&str> {
    match v {
        Bson::String(s) | Bson::Symbol(s) | Bson::JavaScriptCode(s) => Some(s),
        Bson::JavaScriptCodeWithScope(c) => Some(&c.code),
        _ => None,
    }
}

/// Python's `ordering._bson_lt(a, b)` — BSON-order strict-less as a single
/// relation. Unlike [`cmp`], this needs no transitivity (it backs `$min` /
/// `$max`, one comparison per write, not a sort), so it covers the types
/// [`is_sortable`] must bar: bool (own rank, `False < True`), Decimal128
/// (unified numeric), NaN (`<` is False both ways), Binary (bytes), Timestamp,
/// Regex (Python `TypeError` → equal type names → False), Min/MaxKey, and the
/// decoded exotic text types. `None` defers (DBPointer's type-name tiebreak;
/// a Decimal128 that fails to classify).
pub fn bson_lt(a: &Bson, b: &Bson) -> Option<bool> {
    if matches!(a, Bson::DbPointer(_)) || matches!(b, Bson::DbPointer(_)) {
        return None;
    }
    let (ra, rb) = (lt_rank(a), lt_rank(b));
    if ra != rb {
        return Some(ra < rb);
    }
    // Same rank. Null / undefined (both decode to None): `None < None` is a
    // TypeError in Python… but `_bson_lt` short-circuits `a is None or b is
    // None` to False first.
    if matches!(a, Bson::Null | Bson::Undefined) || matches!(b, Bson::Null | Bson::Undefined) {
        return Some(false);
    }
    match (a, b) {
        (Bson::Boolean(x), Bson::Boolean(y)) => Some(x < y),
        (Bson::DateTime(x), Bson::DateTime(y)) => Some(x.timestamp_millis() < y.timestamp_millis()),
        (Bson::Timestamp(x), Bson::Timestamp(y)) => {
            Some((x.time, x.increment) < (y.time, y.increment))
        }
        (Bson::ObjectId(x), Bson::ObjectId(y)) => Some(x.bytes() < y.bytes()),
        (Bson::Binary(x), Bson::Binary(y)) => Some(x.bytes < y.bytes),
        // Two regexes: Python `<` raises TypeError → equal type names → False.
        (Bson::RegularExpression(_), Bson::RegularExpression(_)) => Some(false),
        (Bson::MinKey, Bson::MinKey) | (Bson::MaxKey, Bson::MaxKey) => Some(false),
        (Bson::Document(x), Bson::Document(y)) => {
            for ((ak, av), (bk, bv)) in x.iter().zip(y.iter()) {
                if ak != bk {
                    return Some(ak < bk);
                }
                if bson_lt(av, bv)? {
                    return Some(true);
                }
                if bson_lt(bv, av)? {
                    return Some(false);
                }
            }
            Some(x.len() < y.len())
        }
        (Bson::Array(x), Bson::Array(y)) => {
            for (av, bv) in x.iter().zip(y.iter()) {
                if bson_lt(av, bv)? {
                    return Some(true);
                }
                if bson_lt(bv, av)? {
                    return Some(false);
                }
            }
            Some(x.len() < y.len())
        }
        _ => {
            if let Some(x) = lt_text(a) {
                return Some(x < lt_text(b)?);
            }
            // Rank 3: the unified numeric type (int / long / double /
            // Decimal128). NaN is unordered → Python `<` is False.
            if let Some(r) = numeric::fast_cmp(a, b) {
                return Some(r == Some(Ordering::Less));
            }
            match (numeric::classify(a), numeric::classify(b)) {
                (Some(na), Some(nb)) => Some(numeric::cmp(&na, &nb) == Some(Ordering::Less)),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{bson, Bson};

    fn b(v: bson::Bson) -> Bson {
        v
    }

    #[test]
    fn cross_type_rank() {
        // null < number < string < bool < date
        assert_eq!(cmp(&Bson::Null, &b(bson!(1))), Ordering::Less);
        assert_eq!(cmp(&b(bson!(5)), &b(bson!("a"))), Ordering::Less);
        assert_eq!(cmp(&b(bson!("z")), &b(bson!(true))), Ordering::Less);
    }

    #[test]
    fn within_type() {
        assert_eq!(cmp(&b(bson!(2)), &b(bson!(3.5))), Ordering::Less);
        assert_eq!(cmp(&b(bson!(1)), &b(bson!(1.0))), Ordering::Equal);
        assert_eq!(cmp(&b(bson!("a")), &b(bson!("b"))), Ordering::Less);
        assert_eq!(cmp(&b(bson!(false)), &b(bson!(true))), Ordering::Less);
        assert_eq!(cmp(&b(bson!([1, 2])), &b(bson!([1, 3]))), Ordering::Less);
        assert_eq!(cmp(&b(bson!([1])), &b(bson!([1, 0]))), Ordering::Less);
    }

    #[test]
    fn nan_is_equal_not_deferred() {
        assert_eq!(cmp(&b(bson!(f64::NAN)), &b(bson!(5))), Ordering::Equal);
    }

    #[test]
    fn sortable_gating() {
        assert!(is_sortable(&b(bson!({"a": [1, "x", {"n": 2}]}))));
        // bool / NaN / Decimal128 defer (their Python `==` diverges from cmp).
        assert!(!is_sortable(&b(bson!(true))));
        assert!(!is_sortable(&b(bson!(f64::NAN))));
        assert!(!is_sortable(&b(bson!([1, "x", true]))));
        assert!(!is_sortable(&Bson::Decimal128("1.5".parse().unwrap())));
        assert!(!is_sortable(&b(
            bson!({"a": Bson::Decimal128("1".parse().unwrap())})
        )));
    }
}

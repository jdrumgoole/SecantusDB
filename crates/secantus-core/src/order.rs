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
        // mongod ranks JavaScript between Regex and MaxKey. Kept in step with
        // `sortkey::RANK_JAVASCRIPT` -- that encoder writes the rank byte
        // persisted index entries are sorted by, and moving one alone makes an
        // index change the sort answer.
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => 125,
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

/// Whether the SORT engines can order this value.
///
/// Now the same predicate as [`is_comparable`], and deliberately so. It used to
/// be much narrower -- excluding NaN, Min/MaxKey, Binary, Timestamp, Regex and
/// Code -- to protect the sort engines from a type with no transitive same-type
/// arm in [`cmp`]. Every one of those arms now EXISTS (NaN ranks below the
/// numbers, two MinKeys are equal, regexes compare by pattern then options,
/// Binary by bytes, Code by source), so the narrow gate no longer protected
/// anything: it just made `Fallback::Defer` fire, and a defer on the standalone
/// Rust server is an ERROR.
///
/// The cost of leaving it stale was a plain `{$sort: {v: 1}}` answering
/// `2 aggregation pipeline uses a stage or operator not supported` for any
/// collection holding a NaN, a MinKey or a MaxKey (measured 8.2.11,
/// 2026-09-09, by `tools/probes/aggregation_stage_results.py`). Ordinary data,
/// ordinary query, whole pipeline refused -- and it took `$group`, `$bucket`
/// and `$topN` down with it, since they sort too.
///
/// `cmp` is total over everything this admits: same-rank pairs all have an arm,
/// cross-rank pairs go through `type_rank`, and the fallback answers `Equal`,
/// which is transitive. That matters more than usual here -- Rust's sort
/// PANICS on a comparator that is not a total order.
pub fn is_sortable(v: &Bson) -> bool {
    is_comparable(v)
}

/// Whether [`cmp`] can order this value.
///
/// The comparison OPERATORS need no transitivity -- one pair, one answer -- and
/// mongod compares every BSON type by its canonical rank. Gating them on
/// `is_sortable` made `{$gt: [BinData(...), 0]}` a `2 query uses a construct
/// the Rust server does not support`, and one Binary / Timestamp / Regex /
/// Code / MinKey / MaxKey / NaN document broke the whole `$expr` query.
/// Measured against 8.2.11 on 2026-09-08: 120 of 399 comparison cells diverged.
///
/// Only `DbPointer` is excluded, matching [`bson_lt`]: its tiebreak is a
/// type-name comparison nobody has measured.
pub fn is_comparable(v: &Bson) -> bool {
    match v {
        Bson::DbPointer(_) => false,
        Bson::Document(d) => d.values().all(is_comparable),
        Bson::Array(a) => a.iter().all(is_comparable),
        _ => true,
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
        // Two regexes compare by pattern, then by option string (probed
        // 8.2.11). This used to report every pair EQUAL, justified by what
        // Python's `<` did -- and Python was wrong: `bson.Regex` defines no
        // `__lt__`, so it fell to a type-name fallback. `$max` over regexes
        // never moved on either server.
        (Bson::RegularExpression(x), Bson::RegularExpression(y)) => {
            crate::regexutil::regex_sort_key(x).cmp(&crate::regexutil::regex_sort_key(y))
        }
        (Bson::Document(x), Bson::Document(y)) => doc_cmp(x, y),
        (Bson::Array(x), Bson::Array(y)) => array_cmp(x, y),
        (Bson::JavaScriptCode(x), Bson::JavaScriptCode(y)) => x.cmp(y),
        (Bson::JavaScriptCodeWithScope(x), Bson::JavaScriptCodeWithScope(y)) => x.code.cmp(&y.code),
        // Rank 3: the unified numeric type. NaN sorts BELOW every other number
        // -- `{$cmp: [NaN, 0]}` is -1 on mongod, and the storage sort already
        // places it there. It used to fall through to the numeric arm below and
        // compare EQUAL to everything, which is Python's `<`-is-false-both-ways
        // and not mongod's order.
        _ if crate::query::is_nan_bson(a) || crate::query::is_nan_bson(b) => {
            match (crate::query::is_nan_bson(a), crate::query::is_nan_bson(b)) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Less,
                _ => Ordering::Greater,
            }
        }
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
/// JavaScript deliberately keeps the STRING rank, which is wrong against
/// mongod (it ranks JS between Regex and MaxKey) and is kept anyway: this
/// function has to agree with `sortkey::encode_value`, the rank byte PERSISTED
/// index entries are sorted by, which ranks Code as a string. Moving one and
/// not the other makes an index change the sort answer; moving both is an
/// on-disk format break. See the matching note in `ordering.py` and the backlog
/// item for the proper fix (a sticky catalog flag that makes the sort picker
/// decline such an index for ordering).
///
/// MATCHING is unaffected: `query::compare_values` brackets JavaScript
/// separately without consulting this, and the exact match pass rechecks every
/// index candidate.
fn lt_rank(v: &Bson) -> u8 {
    match v {
        // A BSON Symbol really IS a string to mongod (and pymongo decodes one
        // as `str`); JavaScript is not, and takes `type_rank`'s own arm.
        //
        // These two used to be the literal numbers 4 and 2 -- Python's ranks, on
        // Python's 1..13 scale -- returned into `type_rank`'s spaced-by-10
        // table, where 4 sits BELOW MinKey (10). Latent until a caller compared
        // such a value against a MinKey / MaxKey bound, which then sorted it
        // under MinKey.
        Bson::Symbol(_) => type_rank(&Bson::String(String::new())),
        Bson::Undefined => type_rank(&Bson::Null),
        _ => type_rank(v),
    }
}

/// The text a rank-4 value compares by: Python sees plain `str` for String /
/// Symbol / JS code (`Code` is a str subclass; a with-scope Code compares by
/// its code string, scope ignored).
fn lt_text(v: &Bson) -> Option<&str> {
    match v {
        Bson::String(s) | Bson::Symbol(s) => Some(s),
        // JavaScript no longer shares a rank with String, so these are only
        // reached for a JS/JS pair, which compares by code text.
        Bson::JavaScriptCode(s) => Some(s),
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
        // Pattern, then option string -- see the note in `cmp`.
        (Bson::RegularExpression(x), Bson::RegularExpression(y)) => {
            Some(crate::regexutil::regex_sort_key(x) < crate::regexutil::regex_sort_key(y))
        }
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
    fn nan_sorts_below_every_other_number() {
        // `{$cmp: [NaN, 5]}` is -1 on mongod 8.2.11, and this project's own
        // storage sort already places NaN between null and the other numbers.
        // This asserted `Equal` -- Python's `<`-is-false-both-ways -- pinning an
        // implementation choice that contradicted both. Re-measured 2026-09-08.
        assert_eq!(cmp(&b(bson!(f64::NAN)), &b(bson!(5))), Ordering::Less);
        assert_eq!(cmp(&b(bson!(5)), &b(bson!(f64::NAN))), Ordering::Greater);
        assert_eq!(
            cmp(&b(bson!(f64::NAN)), &b(bson!(f64::NAN))),
            Ordering::Equal
        );
        assert_eq!(
            cmp(&b(bson!(f64::NAN)), &b(bson!(f64::NEG_INFINITY))),
            Ordering::Less
        );
    }

    /// The comparison OPERATORS accept every type `cmp` ranks, which is wider
    /// than the sort engines' `is_sortable`.
    #[test]
    fn sortable_and_comparable_admit_the_same_values() {
        // These two were once different predicates, `is_sortable` being the
        // narrower one. They are the same now: every type it excluded has a
        // transitive same-type arm in `cmp`, so the narrow gate protected
        // nothing and only made `Fallback::Defer` fire -- which on the
        // standalone Rust server is an error. See `is_sortable`.
        for v in [
            b(bson!(f64::NAN)),
            Bson::MinKey,
            Bson::MaxKey,
            Bson::Timestamp(bson::Timestamp {
                time: 1,
                increment: 1,
            }),
            Bson::Decimal128("NaN".parse().unwrap()),
        ] {
            assert!(is_comparable(&v), "{v:?} should be comparable");
            assert!(is_sortable(&v), "{v:?} should be sortable");
        }
    }

    #[test]
    fn sortable_gating() {
        assert!(is_sortable(&b(bson!({"a": [1, "x", {"n": 2}]}))));
        // bool / NaN / Decimal128 defer (their Python `==` diverges from cmp).
        // Bools ARE sortable: mongod ranks them above ObjectId and below Date,
        // and `{$gt: [true, 1]}` is true. This asserted the opposite, pinning a
        // gating decision rather than a behaviour.
        assert!(is_sortable(&b(bson!(true))));
        // NaN IS sortable, for the third time in this test's history and for
        // the same reason bools and Decimal128 turned out to be: it asserted a
        // gating decision, not a behaviour. mongod sorts a collection holding a
        // NaN without complaint and ranks it below every other number, and
        // refusing it here made a plain `{$sort: {v: 1}}` fail outright
        // (measured 8.2.11, 2026-09-09).
        assert!(is_sortable(&b(bson!(f64::NAN))));
        // An array of sortable elements is sortable, and a bool is now one of
        // them; this case existed only because bools were excluded.
        assert!(is_sortable(&b(bson!([1, "x", true]))));
        // Decimal128 IS sortable, for the same reason bools turned out to be:
        // this asserted a gating decision, not a behaviour. mongod interleaves
        // decimals with the other numerics -- a mixed field sorts
        // `NaN, Decimal128("1"), 2, Decimal128("2.5"), 3.0, "s"` (probed
        // 8.2.11, 2026-09-02) -- and `cmp` has always routed rank 3 through
        // `numeric::classify`, which handles them.
        assert!(is_sortable(&Bson::Decimal128("1.5".parse().unwrap())));
        assert!(is_sortable(&b(
            bson!({"a": Bson::Decimal128("1".parse().unwrap())})
        )));
        // ... and a decimal NaN alongside the double one.
        assert!(is_sortable(&Bson::Decimal128("NaN".parse().unwrap())));
    }
}

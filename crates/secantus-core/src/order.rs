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
fn type_rank(v: &Bson) -> u8 {
    match v {
        Bson::MinKey => 1,
        Bson::Null => 2,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => 3,
        Bson::String(_) => 4,
        Bson::Document(_) => 5,
        Bson::Array(_) => 6,
        Bson::Binary(_) => 7,
        Bson::ObjectId(_) => 8,
        Bson::Boolean(_) => 9,
        Bson::DateTime(_) => 10,
        Bson::Timestamp(_) => 11,
        Bson::RegularExpression(_) => 12,
        Bson::MaxKey => 13,
        _ => 5, // matches Python's `return 5` fallback (never reached: is_sortable bars these)
    }
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
        _ => match (numeric::classify(a), numeric::classify(b)) {
            (Some(na), Some(nb)) => numeric::cmp(&na, &nb).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        },
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

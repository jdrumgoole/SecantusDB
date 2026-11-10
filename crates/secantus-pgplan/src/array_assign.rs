//! Assignment to an array element or slice: `UPDATE t SET a[i] = v` and
//! PL/pgSQL's `x[i] := v`.
//!
//! A port of PostgreSQL 15's `array_subscript_assign` /
//! `array_subscript_assign_slice` (`arraysubs.c`) over `array_set_element`
//! / `array_set_slice` (`arrayfuncs.c`), checks in their order and errors in
//! their words. In brief, as measured on 15.19:
//!
//! - a NULL subscript is 22004; a NULL array is an empty one, so assigning
//!   into it builds an array whose lower bounds are the subscripts given
//!   (`x[1][2] := 5` over NULL is `[1:1][2:2]={{5}}`);
//! - a one-dimensional array grows to meet a subscript below or past its
//!   bounds, the gap NULL-filled; a multidimensional one does not grow
//!   (2202E `array subscript out of range`);
//! - a slice whose source is NULL leaves the array as it was; its source is
//!   read serially, whatever its shape, and must hold at least as many
//!   elements as the slice (2202E `source array too small`);
//! - once any subscript is a slice, a plain one `[n]` is the slice `[1:n]`
//!   (`transformContainerSubscripts`).
//!
//! The assigned value arrives already coerced to the element type (or, for
//! a slice, the array type); the caller owns that coercion.

use crate::arrays::{
    bounded, dim_lengths, flatten, is_one_dimensional_empty, lower_bounds, strip, subscript_index,
    MAXDIM, MAX_ARRAY_SIZE,
};
use crate::{Error, Result};
use bson::Bson;

/// One subscript of an assignment target, its bound(s) evaluated. In a
/// slice, `None` is a bound left out (`x[2:]`) and `Some(Bson::Null)` a
/// NULL one.
#[derive(Debug, Clone, PartialEq)]
pub enum AssignSubscript {
    Index(Bson),
    Slice(Option<Bson>, Option<Bson>),
}

/// `current` with the element or slice `subs` names set to `value`.
pub fn assign_subscripts(current: Bson, subs: &[AssignSubscript], value: Bson) -> Result<Bson> {
    // `ExecEvalSubscriptingRef` checks every subscript before the
    // assignment runs: an assignment's subscript may not be NULL.
    let is_slice = subs.iter().any(|s| matches!(s, AssignSubscript::Slice(..)));
    if is_slice {
        let mut lower = Vec::with_capacity(subs.len());
        let mut upper = Vec::with_capacity(subs.len());
        for s in subs {
            match s {
                // A plain subscript among slices is the upper bound of a
                // slice starting at 1.
                AssignSubscript::Index(v) => {
                    lower.push(Some(1));
                    upper.push(Some(int4_subscript(v)?));
                }
                AssignSubscript::Slice(lo, hi) => {
                    lower.push(lo.as_ref().map(int4_subscript).transpose()?);
                    upper.push(hi.as_ref().map(int4_subscript).transpose()?);
                }
            }
        }
        set_slice(current, &lower, &upper, value)
    } else {
        let mut index = Vec::with_capacity(subs.len());
        for s in subs {
            if let AssignSubscript::Index(v) = s {
                index.push(int4_subscript(v)?);
            }
        }
        set_element(current, &index, value)
    }
}

/// `ExecEvalSubscriptingRef`'s check of an assignment's subscripts, which
/// runs before the value assigned is evaluated: none may be NULL.
pub fn check_subscripts(subs: &[AssignSubscript]) -> Result<()> {
    for s in subs {
        match s {
            AssignSubscript::Index(v) => {
                int4_subscript(v)?;
            }
            AssignSubscript::Slice(lo, hi) => {
                for v in [lo, hi].into_iter().flatten() {
                    int4_subscript(v)?;
                }
            }
        }
    }
    Ok(())
}

/// A subscript as the `int4` PostgreSQL coerces it to.
fn int4_subscript(v: &Bson) -> Result<i64> {
    if matches!(v, Bson::Null) {
        return Err(Error::Sqlstate(
            "22004",
            "array subscript in assignment must not be null".into(),
        ));
    }
    let i = subscript_index(v)?;
    if i < i64::from(i32::MIN) || i > i64::from(i32::MAX) {
        return Err(Error::NumericOutOfRange("integer out of range".into()));
    }
    Ok(i)
}

fn subscript_error(message: &str) -> Error {
    Error::Sqlstate("2202E", message.into())
}

fn size_exceeded() -> Error {
    Error::Sqlstate(
        "54000",
        format!("array size exceeds the maximum allowed ({MAX_ARRAY_SIZE})"),
    )
}

/// `ArrayGetNItems`: the element count of `dims`, refused past
/// `MaxArraySize` (a negative dimension is an overflowed one).
fn n_items(dims: &[i64]) -> Result<i64> {
    let mut product: i64 = 1;
    for &d in dims {
        if !(0..=MAX_ARRAY_SIZE).contains(&d) {
            return Err(size_exceeded());
        }
        product = product.saturating_mul(d);
        if product > MAX_ARRAY_SIZE {
            return Err(size_exceeded());
        }
    }
    Ok(if dims.is_empty() { 0 } else { product })
}

/// `ArrayCheckBounds`: each dimension's upper bound must fit an `int4`.
fn check_bounds(dims: &[i64], lbs: &[i64]) -> Result<()> {
    for (d, lb) in dims.iter().zip(lbs) {
        if d + lb > i64::from(i32::MAX) {
            return Err(Error::Sqlstate(
                "54000",
                format!("array lower bound is too large: {lb}"),
            ));
        }
    }
    Ok(())
}

/// An array as its dimensions, lower bounds and elements in row-major
/// order. A NULL or an empty array has no dimension.
fn shape(v: &Bson) -> (Vec<i64>, Vec<i64>, Vec<Bson>) {
    if matches!(v, Bson::Null) {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    if is_one_dimensional_empty(v) {
        return (vec![0], vec![1], Vec::new());
    }
    let plain = strip(v);
    let dims: Vec<i64> = dim_lengths(&plain).into_iter().map(|d| d as i64).collect();
    let mut lbs = lower_bounds(v);
    lbs.resize(dims.len(), 1);
    (dims, lbs, flatten(&plain))
}

/// The array of `dims` with lower bounds `lbs` holding `items` (row-major).
fn build(dims: &[i64], lbs: &[i64], items: Vec<Bson>) -> Bson {
    fn nest(dims: &[i64], items: &mut std::vec::IntoIter<Bson>) -> Bson {
        match dims.split_first() {
            None => items.next().unwrap_or(Bson::Null),
            Some((&d, rest)) => Bson::Array((0..d).map(|_| nest(rest, items)).collect()),
        }
    }
    if items.is_empty() {
        return Bson::Array(Vec::new());
    }
    bounded(nest(dims, &mut items.into_iter()), lbs)
}

/// The row-major offset of the element at `index`.
fn offset(dims: &[i64], lbs: &[i64], index: &[i64]) -> usize {
    let mut off = 0i64;
    for ((d, lb), i) in dims.iter().zip(lbs).zip(index) {
        off = off * d + (i - lb);
    }
    off as usize
}

/// `array_set_element`.
fn set_element(current: Bson, index: &[i64], value: Bson) -> Result<Bson> {
    let n = index.len();
    if n == 0 || n > MAXDIM {
        return Err(subscript_error("wrong number of array subscripts"));
    }
    let (mut dims, mut lbs, items) = shape(&current);
    // An empty array: one of `n` dimensions, each of length 1 and starting
    // at its subscript.
    if dims.is_empty() {
        let dims = vec![1; n];
        n_items(&dims)?;
        check_bounds(&dims, index)?;
        return Ok(build(&dims, index, vec![value]));
    }
    if dims.len() != n {
        return Err(subscript_error("wrong number of array subscripts"));
    }
    let (mut before, mut after) = (0i64, 0i64);
    if n == 1 {
        if index[0] < lbs[0] {
            before = lbs[0] - index[0];
            dims[0] += before;
            lbs[0] = index[0];
        }
        if index[0] >= dims[0] + lbs[0] {
            after = index[0] - (dims[0] + lbs[0]) + 1;
            dims[0] += after;
        }
    } else {
        // PostgreSQL does not extend a multidimensional array.
        for ((i, d), lb) in index.iter().zip(&dims).zip(&lbs) {
            if i < lb || *i >= d + lb {
                return Err(subscript_error("array subscript out of range"));
            }
        }
    }
    n_items(&dims)?;
    check_bounds(&dims, &lbs)?;
    let mut out = Vec::with_capacity((before + items.len() as i64 + after) as usize);
    out.resize(before as usize, Bson::Null);
    out.extend(items);
    out.resize(out.len() + after as usize, Bson::Null);
    let at = offset(&dims, &lbs, index);
    out[at] = value;
    Ok(build(&dims, &lbs, out))
}

/// `array_set_slice`; `None` is a bound the slice leaves out.
fn set_slice(
    current: Bson,
    lower: &[Option<i64>],
    upper: &[Option<i64>],
    value: Bson,
) -> Result<Bson> {
    // `array_subscript_assign_slice` makes a NULL array an empty one before
    // `array_set_slice` returns it untouched for a NULL source.
    if matches!(value, Bson::Null) {
        return Ok(match current {
            Bson::Null => Bson::Array(Vec::new()),
            other => other,
        });
    }
    let source = flatten(&strip(&value));
    let n = lower.len();
    let (mut dims, mut lbs, items) = shape(&current);
    if dims.is_empty() {
        let mut new_dims = Vec::with_capacity(n);
        let mut new_lbs = Vec::with_capacity(n);
        for (lo, hi) in lower.iter().zip(upper) {
            let (Some(lo), Some(hi)) = (lo, hi) else {
                return Err(subscript_error(
                    "array slice subscript must provide both boundaries\nDetail: When \
                     assigning to a slice of an empty array value, slice boundaries must be \
                     fully specified.",
                ));
            };
            let d = hi - lo + 1;
            if d < i64::from(i32::MIN) || d > i64::from(i32::MAX) {
                return Err(size_exceeded());
            }
            new_dims.push(d);
            new_lbs.push(*lo);
        }
        let need = n_items(&new_dims)?;
        if (source.len() as i64) < need {
            return Err(subscript_error("source array too small"));
        }
        check_bounds(&new_dims, &new_lbs)?;
        if need == 0 {
            return Ok(Bson::Array(Vec::new()));
        }
        return Ok(build(
            &new_dims,
            &new_lbs,
            source.into_iter().take(need as usize).collect(),
        ));
    }
    let ndim = dims.len();
    if ndim < n || ndim > MAXDIM {
        return Err(subscript_error("wrong number of array subscripts"));
    }
    let mut lo = vec![0i64; ndim];
    let mut hi = vec![0i64; ndim];
    let (mut before, mut after) = (0i64, 0i64);
    if ndim == 1 {
        lo[0] = lower[0].unwrap_or(lbs[0]);
        hi[0] = upper[0].unwrap_or(dims[0] + lbs[0] - 1);
        if lo[0] > hi[0] {
            return Err(subscript_error(
                "upper bound cannot be less than lower bound",
            ));
        }
        if lo[0] < lbs[0] {
            before = lbs[0] - lo[0];
            dims[0] += before;
            lbs[0] = lo[0];
        }
        if hi[0] >= dims[0] + lbs[0] {
            after = hi[0] - (dims[0] + lbs[0]) + 1;
            dims[0] += after;
        }
    } else {
        // PostgreSQL does not extend a multidimensional array.
        for i in 0..n {
            lo[i] = lower[i].unwrap_or(lbs[i]);
            hi[i] = upper[i].unwrap_or(dims[i] + lbs[i] - 1);
            if lo[i] > hi[i] {
                return Err(subscript_error(
                    "upper bound cannot be less than lower bound",
                ));
            }
            if lo[i] < lbs[i] || hi[i] >= dims[i] + lbs[i] {
                return Err(subscript_error("array subscript out of range"));
            }
        }
        // A dimension the subscripts leave out is taken whole.
        for i in n..ndim {
            lo[i] = lbs[i];
            hi[i] = dims[i] + lbs[i] - 1;
            if lo[i] > hi[i] {
                return Err(subscript_error(
                    "upper bound cannot be less than lower bound",
                ));
            }
        }
    }
    n_items(&dims)?;
    check_bounds(&dims, &lbs)?;
    let span: Vec<i64> = lo.iter().zip(&hi).map(|(l, h)| h - l + 1).collect();
    let needed = n_items(&span)?;
    if needed > source.len() as i64 {
        return Err(subscript_error("source array too small"));
    }
    let mut out = Vec::with_capacity((before + items.len() as i64 + after) as usize);
    out.resize(before as usize, Bson::Null);
    out.extend(items);
    out.resize(out.len() + after as usize, Bson::Null);
    // Every position of the slice, in row-major order, takes the source's
    // next element.
    let mut at = lo.clone();
    for item in source.into_iter().take(needed as usize) {
        out[offset(&dims, &lbs, &at)] = item;
        for d in (0..ndim).rev() {
            if at[d] < hi[d] {
                at[d] += 1;
                break;
            }
            at[d] = lo[d];
        }
    }
    Ok(build(&dims, &lbs, out))
}

#[cfg(test)]
mod tests {
    use super::{assign_subscripts, AssignSubscript as S};
    use crate::arrays::bounded;
    use bson::{bson, Bson};

    fn i(n: i32) -> Bson {
        Bson::Int32(n)
    }

    fn idx(ns: &[i32]) -> Vec<S> {
        ns.iter().map(|n| S::Index(i(*n))).collect()
    }

    fn sqlstate(r: crate::Result<Bson>) -> (&'static str, String) {
        let e = r.expect_err("an error");
        (e.sqlstate(), e.to_string())
    }

    // Each expectation below is PostgreSQL 15.19's answer to the PL/pgSQL
    // assignment in its comment.

    #[test]
    fn an_element_assignment_grows_a_one_dimensional_array() {
        // x[1] := 5 over NULL
        assert_eq!(
            assign_subscripts(Bson::Null, &idx(&[1]), i(5)).unwrap(),
            bson!([5])
        );
        // x[4] := 5 over {1,2}
        assert_eq!(
            assign_subscripts(bson!([1, 2]), &idx(&[4]), i(5)).unwrap(),
            bson!([1, 2, Bson::Null, 5])
        );
        // x[0] := 9 over {1,2} is [0:2]={9,1,2}
        assert_eq!(
            assign_subscripts(bson!([1, 2]), &idx(&[0]), i(9)).unwrap(),
            bounded(bson!([9, 1, 2]), &[0])
        );
        // x[4] := 3 over NULL is [4:4]={3}
        assert_eq!(
            assign_subscripts(Bson::Null, &idx(&[4]), i(3)).unwrap(),
            bounded(bson!([3]), &[4])
        );
        // x[1] := 5 over [0:1]={1,2} is [0:1]={1,5}
        assert_eq!(
            assign_subscripts(bounded(bson!([1, 2]), &[0]), &idx(&[1]), i(5)).unwrap(),
            bounded(bson!([1, 5]), &[0])
        );
    }

    #[test]
    fn a_multidimensional_array_does_not_grow() {
        let m = bson!([[1, 2], [3, 4]]);
        assert_eq!(
            assign_subscripts(m.clone(), &idx(&[2, 1]), i(9)).unwrap(),
            bson!([[1, 2], [9, 4]])
        );
        assert_eq!(
            sqlstate(assign_subscripts(m.clone(), &idx(&[3, 1]), i(9))),
            ("2202E", "array subscript out of range".into())
        );
        assert_eq!(
            sqlstate(assign_subscripts(m, &idx(&[1]), i(9))),
            ("2202E", "wrong number of array subscripts".into())
        );
        // x[1][2][3] := 5 over NULL is [1:1][2:2][3:3]={{{5}}}
        assert_eq!(
            assign_subscripts(Bson::Null, &idx(&[1, 2, 3]), i(5)).unwrap(),
            bounded(bson!([[[5]]]), &[1, 2, 3])
        );
        // x[1][0] := 5 over [0:1][0:1]={{1,2},{3,4}}
        assert_eq!(
            assign_subscripts(
                bounded(bson!([[1, 2], [3, 4]]), &[0, 0]),
                &idx(&[1, 0]),
                i(5)
            )
            .unwrap(),
            bounded(bson!([[1, 2], [5, 4]]), &[0, 0])
        );
    }

    #[test]
    fn subscript_limits() {
        assert_eq!(
            sqlstate(assign_subscripts(bson!([1]), &[S::Index(Bson::Null)], i(1))),
            (
                "22004",
                "array subscript in assignment must not be null".into()
            )
        );
        assert_eq!(
            sqlstate(assign_subscripts(bson!([1]), &idx(&[i32::MAX]), i(1))),
            (
                "54000",
                "array size exceeds the maximum allowed (134217727)".into()
            )
        );
        assert_eq!(
            sqlstate(assign_subscripts(Bson::Null, &idx(&[i32::MAX]), i(1))),
            ("54000", "array lower bound is too large: 2147483647".into())
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                bson!([1]),
                &[S::Index(Bson::Int64(3_000_000_000))],
                i(1)
            )),
            ("22003", "integer out of range".into())
        );
    }

    fn slice(lo: Option<i32>, hi: Option<i32>) -> S {
        S::Slice(lo.map(i), hi.map(i))
    }

    #[test]
    fn a_slice_assignment() {
        let a = bson!([1, 2, 3]);
        assert_eq!(
            assign_subscripts(a.clone(), &[slice(Some(2), Some(3))], bson!([8, 9])).unwrap(),
            bson!([1, 8, 9])
        );
        assert_eq!(
            assign_subscripts(a.clone(), &[slice(Some(2), None)], bson!([7, 8])).unwrap(),
            bson!([1, 7, 8])
        );
        assert_eq!(
            assign_subscripts(a.clone(), &[slice(None, Some(2))], bson!([7, 8])).unwrap(),
            bson!([7, 8, 3])
        );
        // The source is read serially, whatever its shape and bounds.
        assert_eq!(
            assign_subscripts(a.clone(), &[slice(Some(0), Some(1))], bson!([[7, 8]])).unwrap(),
            bounded(bson!([7, 8, 2, 3]), &[0])
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                a.clone(),
                &[slice(Some(1), Some(2))],
                bson!([9])
            )),
            ("2202E", "source array too small".into())
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                a.clone(),
                &[slice(Some(3), Some(2))],
                bson!([8, 9])
            )),
            (
                "2202E",
                "upper bound cannot be less than lower bound".into()
            )
        );
        // A NULL source leaves the array as it was; a NULL array becomes
        // an empty one.
        assert_eq!(
            assign_subscripts(a.clone(), &[slice(Some(2), Some(3))], Bson::Null).unwrap(),
            a
        );
        assert_eq!(
            assign_subscripts(Bson::Null, &[slice(Some(1), Some(2))], Bson::Null).unwrap(),
            bson!([])
        );
    }

    #[test]
    fn a_slice_of_an_empty_array() {
        assert_eq!(
            assign_subscripts(Bson::Null, &[slice(Some(2), Some(3))], bson!([8, 9, 10])).unwrap(),
            bounded(bson!([8, 9]), &[2])
        );
        assert_eq!(
            assign_subscripts(Bson::Null, &[slice(Some(3), Some(2))], bson!([8])).unwrap(),
            bson!([])
        );
        let (state, message) = sqlstate(assign_subscripts(
            Bson::Null,
            &[slice(Some(2), None)],
            bson!([8, 9]),
        ));
        assert_eq!(state, "2202E");
        assert!(message.starts_with("array slice subscript must provide both boundaries"));
        // x[1:2][3:3] := '{5,6}' over NULL is [1:2][3:3]={{5},{6}}
        assert_eq!(
            assign_subscripts(
                Bson::Null,
                &[slice(Some(1), Some(2)), slice(Some(3), Some(3))],
                bson!([5, 6])
            )
            .unwrap(),
            bounded(bson!([[5], [6]]), &[1, 3])
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                Bson::Null,
                &[slice(Some(i32::MIN), Some(i32::MAX))],
                bson!([1])
            )),
            (
                "54000",
                "array size exceeds the maximum allowed (134217727)".into()
            )
        );
    }

    #[test]
    fn a_slice_of_a_multidimensional_array() {
        let m = bson!([[1, 2], [3, 4]]);
        // x[1:1][1:2] := '{{8,9}}'
        assert_eq!(
            assign_subscripts(
                m.clone(),
                &[slice(Some(1), Some(1)), slice(Some(1), Some(2))],
                bson!([[8, 9]])
            )
            .unwrap(),
            bson!([[8, 9], [3, 4]])
        );
        // x[2:2] := '{8,9}': the second dimension is taken whole.
        assert_eq!(
            assign_subscripts(m.clone(), &[slice(Some(2), Some(2))], bson!([8, 9])).unwrap(),
            bson!([[1, 2], [8, 9]])
        );
        // x[2][1:2] is x[1:2][1:2]: four elements wanted.
        assert_eq!(
            sqlstate(assign_subscripts(
                m.clone(),
                &[S::Index(i(2)), slice(Some(1), Some(2))],
                bson!([8, 9])
            )),
            ("2202E", "source array too small".into())
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                m,
                &[slice(Some(1), Some(3)), S::Index(i(1))],
                bson!([8, 9, 7])
            )),
            ("2202E", "array subscript out of range".into())
        );
        assert_eq!(
            sqlstate(assign_subscripts(
                bson!([1, 2]),
                &[slice(Some(1), Some(1)), slice(Some(1), Some(1))],
                bson!([9])
            )),
            ("2202E", "wrong number of array subscripts".into())
        );
    }
}

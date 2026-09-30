//! PostgreSQL's array functions, containment operators and subscripting.
//!
//! An array is a `Bson::Array`, and a multidimensional one is nested: PG's
//! `ARRAY[[1,2],[3,4]]` is `[[1,2],[3,4]]`. PostgreSQL arrays are RECTANGULAR
//! — every element of a dimension has the same length — so the shape is read
//! off the first element at each level and needs no per-element check.
//!
//! **Lower bounds.** Nearly every array starts at subscript 1 and is a plain
//! `Bson::Array`. One that does not -- a `'[0:1]={a,b}'` literal,
//! `array_fill(v, dims, lbounds)`, an assignment below or past its bounds --
//! is a tagged document (`bounded`) carrying a lower bound per dimension, so
//! its text form (`[3:4]={7,7}`), its subscripts, `array_lower` /
//! `array_dims` and equality all agree with PostgreSQL. Functions that have
//! no use for the bounds see the plain array (`strip`); the ones that keep
//! them -- `array_append`, `array_cat`, `array_prepend` and friends -- put
//! them back on their result, as PostgreSQL does.
//!
//! Two equality rules live here, and they are NOT the same one — measured on
//! PostgreSQL 14.13, because the difference is the kind of thing a reader
//! would call a bug:
//!
//! - `array_position` / `array_positions` / `array_remove` / `array_replace`
//!   search with NULL matching NULL, so `array_position(ARRAY[1,NULL], NULL)`
//!   is 2 and `array_remove(ARRAY[1,NULL], NULL)` is `{1}`.
//! - `@>` / `<@` / `&&` use the element type's `=`, under which NULL matches
//!   nothing — so `ARRAY[1,NULL] @> ARRAY[NULL]` is FALSE, and
//!   `ARRAY[1,NULL] <@ ARRAY[1,NULL]` is false as well.

use crate::{compare_constants, Error, Result};
use bson::Bson;

/// The array built-ins this module answers.
pub const ARRAY_NAMES: &[&str] = &[
    "array_length",
    "array_ndims",
    "array_dims",
    "array_lower",
    "array_upper",
    "cardinality",
    "array_cat",
    "array_append",
    "array_prepend",
    "array_to_string",
    "string_to_array",
    "array_position",
    "array_positions",
    "array_remove",
    "array_replace",
    "array_fill",
    "trim_array",
];

/// Is this one of the array built-ins?
pub fn is_array_function(name: &str) -> bool {
    ARRAY_NAMES.contains(&name)
}

/// The array built-ins whose result type is fixed regardless of the arguments.
///
/// The rest answer an array whose type is one of their ARGUMENTS' — there is
/// no static answer for `array_remove(x, 1)` without looking at `x` — so they
/// are typed by `static_type` from the call instead.
pub fn static_result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "array_length" | "array_ndims" | "array_lower" | "array_upper" | "cardinality"
        | "array_position" => "int4",
        "array_dims" | "array_to_string" => "text",
        "array_positions" => "int4[]",
        "string_to_array" => "text[]",
        _ => return None,
    })
}

/// PostgreSQL's own ceiling on an array's element count, and the number its
/// error message quotes.
///
/// It is enforced here because the two constructors that SIZE an array from a
/// user-supplied number -- `array_fill(v, ARRAY[n])` and `SET a[n] = v` --
/// would otherwise allocate whatever `n` asked for. `SET a[1000000000] = 1` is
/// a one-line statement; PostgreSQL answers it with this error, and a server
/// that instead tried to build the array would be trivially exhaustible.
pub const MAX_ARRAY_SIZE: i64 = 134_217_727;

/// Refuse an element count PostgreSQL would refuse, in its own words.
pub fn check_array_size(n: i64) -> Result<()> {
    if n > MAX_ARRAY_SIZE {
        return Err(Error::DataException(format!(
            "array size exceeds the maximum allowed ({MAX_ARRAY_SIZE})"
        )));
    }
    Ok(())
}

fn wrong_args(name: &str) -> Error {
    Error::Parse(format!(
        "function {name} does not exist with that argument list"
    ))
}

/// The length of each dimension, outermost first. Empty for a NULL, a
/// non-array, or an empty array — all three of which PostgreSQL reports as a
/// zero-dimensional array through `array_ndims` / `array_dims`.
pub fn dim_lengths(v: &Bson) -> Vec<usize> {
    let mut dims = Vec::new();
    let mut cur = v;
    while let Bson::Array(items) = cur {
        let Some(first) = items.first() else { break };
        dims.push(items.len());
        cur = first;
    }
    dims
}

/// Every leaf element, in row-major order. `[[1,2],[3,4]]` is `[1,2,3,4]`.
pub fn flatten(v: &Bson) -> Vec<Bson> {
    let mut out = Vec::new();
    fn walk(v: &Bson, out: &mut Vec<Bson>) {
        match v {
            Bson::Array(items) => items.iter().for_each(|i| walk(i, out)),
            other => out.push(other.clone()),
        }
    }
    walk(v, &mut out);
    out
}

/// The array an argument must be, or an error naming the function.
fn need_array<'a>(name: &str, v: &'a Bson) -> Result<&'a Vec<Bson>> {
    match v {
        Bson::Array(items) => Ok(items),
        _ => Err(wrong_args(name)),
    }
}

/// Equality as the SEARCH functions use it: NULL matches NULL.
fn same(a: &Bson, b: &Bson) -> bool {
    match (a == &Bson::Null, b == &Bson::Null) {
        (true, true) => true,
        (true, false) | (false, true) => false,
        (false, false) => compare_constants(a, b) == Some(std::cmp::Ordering::Equal),
    }
}

/// Equality as the CONTAINMENT operators use it: a NULL matches nothing, not
/// even another NULL.
fn eq(a: &Bson, b: &Bson) -> bool {
    a != &Bson::Null
        && b != &Bson::Null
        && compare_constants(a, b) == Some(std::cmp::Ordering::Equal)
}

/// `a @> b` — does every element of `b` appear in `a`?
///
/// Both sides are FLATTENED first: PostgreSQL's containment ignores
/// dimensionality, so `ARRAY[[1,2],[3,4]] @> ARRAY[3]` is true.
pub fn contains(a: &Bson, b: &Bson) -> bool {
    let (outer, inner) = (flatten(a), flatten(b));
    inner.iter().all(|x| outer.iter().any(|y| eq(x, y)))
}

/// `a && b` — do the two share any element?
pub fn overlaps(a: &Bson, b: &Bson) -> bool {
    let (x, y) = (flatten(a), flatten(b));
    x.iter().any(|i| y.iter().any(|j| eq(i, j)))
}

/// One element of a multi-level array by 1-based subscripts, or NULL for any
/// subscript outside the array.
///
/// A subscript list SHORTER than the array's dimensionality is NULL, not the
/// inner array: `(ARRAY[[1,2],[3,4]])[1]` is NULL on PostgreSQL, which is the
/// surprise worth pinning here.
pub fn element(v: &Bson, subs: &[Option<i64>], ndims: usize) -> Bson {
    if subs.len() != ndims {
        return Bson::Null;
    }
    let mut cur = v;
    for s in subs {
        let Some(i) = s else { return Bson::Null };
        let Bson::Array(items) = cur else {
            return Bson::Null;
        };
        if *i < 1 {
            return Bson::Null;
        }
        match items.get((*i - 1) as usize) {
            Some(next) => cur = next,
            None => return Bson::Null,
        }
    }
    cur.clone()
}

/// A slice of a multi-level array by 1-based, inclusive `(lower, upper)` pairs
/// — one per dimension, each clamped to the array.
///
/// A bound outside the array is clamped rather than an error, and a reversed
/// or wholly-outside range gives the EMPTY array: `(ARRAY[1,2,3])[5:9]` is
/// `{}` where `(ARRAY[1,2,3])[9]` is NULL. A NULL bound makes the whole slice
/// NULL.
pub fn slice(v: &Bson, bounds: &[(Option<i64>, Option<i64>)]) -> Option<Bson> {
    if bounds.iter().any(|(l, u)| l.is_none() || u.is_none()) {
        return None;
    }
    let Bson::Array(items) = v else {
        return Some(Bson::Array(Vec::new()));
    };
    let Some(((lo, hi), rest)) = bounds.split_first() else {
        return Some(v.clone());
    };
    let (lo, hi) = (lo.unwrap_or(1).max(1), hi.unwrap_or(0));
    if hi < lo {
        return Some(Bson::Array(Vec::new()));
    }
    let hi = (hi as usize).min(items.len());
    let lo = lo as usize;
    if lo > hi {
        return Some(Bson::Array(Vec::new()));
    }
    let taken = items[lo - 1..hi].iter();
    Some(Bson::Array(if rest.is_empty() {
        taken.cloned().collect()
    } else {
        taken.map(|i| slice(i, rest)).collect::<Option<Vec<_>>>()?
    }))
}

/// Evaluate an array built-in.
///
/// Every one of these handles NULL itself rather than propagating it, because
/// PostgreSQL's array functions disagree about NULL far more than the scalar
/// ones do: `array_cat(NULL, ARRAY[3])` is `{3}`, `array_remove(NULL, 1)` is
/// NULL, and `array_position(a, NULL)` searches for the NULL.
pub fn call(name: &str, args: &[Bson]) -> Result<Bson> {
    // `array_cat(arr, '{3}')`: an untyped literal beside an array takes that
    // array's type, as PostgreSQL resolves the `anyarray` pair.
    if name == "array_cat" && args.len() == 2 {
        let literal = |v: &Bson| matches!(v, Bson::String(t) if t.trim_start().starts_with(['{', '[']));
        let typed = |v: &Bson| matches!(v, Bson::Array(_)) || is_bounded(v);
        let coerce = |lit: &Bson, other: &Bson| -> Result<Bson> {
            crate::cast_value(lit.clone(), crate::inferred_type(other))
        };
        if literal(&args[1]) && typed(&args[0]) {
            return call(name, &[args[0].clone(), coerce(&args[1], &args[0])?]);
        }
        if literal(&args[0]) && typed(&args[1]) {
            return call(name, &[coerce(&args[0], &args[1])?, args[1].clone()]);
        }
    }
    if name != "array_fill" && !args.iter().any(is_bounded) {
        return call_plain(name, args);
    }
    let plain: Vec<Bson> = args.iter().map(strip).collect();
    let first_lbs = || lower_bounds(args.first().unwrap_or(&Bson::Null));
    match name {
        "array_lower" | "array_upper" => {
            let len = call_plain("array_length", &plain)?;
            let Bson::Int32(len) = len else { return Ok(len) };
            let d = as_i64(&plain[1]).unwrap_or(1) as usize;
            let lb = first_lbs().get(d - 1).copied().unwrap_or(1);
            let v = if name == "array_lower" { lb } else { lb + i64::from(len) - 1 };
            Ok(Bson::Int32(i32::try_from(v).unwrap_or(i32::MAX)))
        }
        "array_dims" => Ok(dims_text(&args[0]).map_or(Bson::Null, Bson::String)),
        "array_append" | "array_remove" | "array_replace" => {
            Ok(bounded(call_plain(name, &plain)?, &first_lbs()))
        }
        "array_cat" => {
            let lbs = if strip(&args[0]).as_array().is_some_and(|a| !a.is_empty()) {
                first_lbs()
            } else {
                lower_bounds(&args[1])
            };
            Ok(bounded(call_plain(name, &plain)?, &lbs))
        }
        // The new first element takes the array's lower bound (measured:
        // `array_prepend('z', '[0:1]={a,b}')` is `[0:2]={z,a,b}`).
        "array_prepend" => Ok(bounded(call_plain(name, &plain)?, &lower_bounds(&args[1]))),
        "array_position" | "array_positions" => {
            // Positions are SUBSCRIPTS, so they move with the lower bound --
            // and so does `array_position`'s optional starting subscript.
            let shift = first_lbs().first().copied().unwrap_or(1) - 1;
            let mut plain = plain;
            if let Some(start) = plain.get_mut(2) {
                if let Some(n) = as_i64(start) {
                    *start = Bson::Int64(n - shift);
                }
            }
            let move_by = |v: Bson| match v {
                Bson::Int32(n) => Bson::Int32(n + shift as i32),
                Bson::Int64(n) => Bson::Int64(n + shift),
                other => other,
            };
            Ok(match call_plain(name, &plain)? {
                Bson::Array(items) => Bson::Array(items.into_iter().map(move_by).collect()),
                other => move_by(other),
            })
        }
        "array_fill" => array_fill(name, &plain),
        _ => call_plain(name, &plain),
    }
}

/// The document keys of an array whose lower bound is not 1 (see
/// `bounded`). PostgreSQL stores a lower bound per dimension; an array whose
/// bounds are all 1 -- nearly every array -- stays a plain `Bson::Array`.
pub const LB_KEY: &str = "__arr_lb";
pub const ITEMS_KEY: &str = "__arr";

/// `v` with lower bounds `lbs` (one per dimension, missing ones 1). A plain
/// array when every bound is 1 or the array is empty.
pub fn bounded(v: Bson, lbs: &[i64]) -> Bson {
    let Bson::Array(items) = v else { return v };
    if items.is_empty() || lbs.iter().all(|l| *l == 1) {
        return Bson::Array(items);
    }
    let ndims = dim_lengths(&Bson::Array(items.clone())).len().max(1);
    let mut lbs = lbs.to_vec();
    lbs.resize(ndims, 1);
    let mut d = bson::Document::new();
    d.insert(LB_KEY, Bson::Array(lbs.into_iter().map(Bson::Int64).collect()));
    d.insert(ITEMS_KEY, Bson::Array(items));
    Bson::Document(d)
}

/// Is `v` an array with a lower bound other than 1?
pub fn is_bounded(v: &Bson) -> bool {
    matches!(v, Bson::Document(d) if d.len() == 2 && d.contains_key(LB_KEY) && d.contains_key(ITEMS_KEY))
}

/// `v` as a plain array, its bounds dropped (anything else unchanged).
pub fn strip(v: &Bson) -> Bson {
    match v {
        Bson::Document(d) if is_bounded(v) => d.get(ITEMS_KEY).cloned().unwrap_or(Bson::Null),
        other => other.clone(),
    }
}

/// The lower bound of each dimension of `v` (empty for a non-array).
pub fn lower_bounds(v: &Bson) -> Vec<i64> {
    match v {
        Bson::Document(d) if is_bounded(v) => d
            .get_array(LB_KEY)
            .map(|a| a.iter().filter_map(as_i64).collect())
            .unwrap_or_default(),
        Bson::Array(_) => vec![1; dim_lengths(v).len()],
        _ => Vec::new(),
    }
}

/// `[lb:ub]` per dimension, as `array_dims` and a bounded array's text
/// form show it; `None` for an empty array or a non-array.
pub fn dims_text(v: &Bson) -> Option<String> {
    let plain = strip(v);
    let dims = dim_lengths(&plain);
    if !matches!(plain, Bson::Array(_)) || dims.is_empty() {
        return None;
    }
    let lbs = lower_bounds(v);
    Some(
        dims.iter()
            .enumerate()
            .map(|(i, n)| {
                let lb = lbs.get(i).copied().unwrap_or(1);
                format!("[{lb}:{}]", lb + *n as i64 - 1)
            })
            .collect(),
    )
}

fn call_plain(name: &str, args: &[Bson]) -> Result<Bson> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Bson::Null);
    match name {
        "array_length" | "array_upper" | "array_lower" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            let (a, d) = (arg(0), arg(1));
            if a == Bson::Null || d == Bson::Null {
                return Ok(Bson::Null);
            }
            need_array(name, &a)?;
            let dims = dim_lengths(&a);
            let Some(d) = as_i64(&d).filter(|d| *d >= 1 && (*d as usize) <= dims.len()) else {
                return Ok(Bson::Null);
            };
            Ok(Bson::Int32(if name == "array_lower" {
                1
            } else {
                i32::try_from(dims[d as usize - 1]).unwrap_or(i32::MAX)
            }))
        }
        "cardinality" => {
            if args.len() != 1 {
                return Err(wrong_args(name));
            }
            let a = arg(0);
            if a == Bson::Null {
                return Ok(Bson::Null);
            }
            need_array(name, &a)?;
            Ok(Bson::Int32(
                i32::try_from(flatten(&a).len()).unwrap_or(i32::MAX),
            ))
        }
        "array_ndims" => {
            if args.len() != 1 {
                return Err(wrong_args(name));
            }
            let a = arg(0);
            if a == Bson::Null {
                return Ok(Bson::Null);
            }
            need_array(name, &a)?;
            let n = dim_lengths(&a).len();
            // A zero-dimensional array (`ARRAY[]::int[]`) has no dimension
            // count to report: PostgreSQL answers NULL, not 0.
            Ok(if n == 0 {
                Bson::Null
            } else {
                Bson::Int32(i32::try_from(n).unwrap_or(i32::MAX))
            })
        }
        "array_dims" => {
            if args.len() != 1 {
                return Err(wrong_args(name));
            }
            let a = arg(0);
            if a == Bson::Null {
                return Ok(Bson::Null);
            }
            need_array(name, &a)?;
            let dims = dim_lengths(&a);
            Ok(if dims.is_empty() {
                Bson::Null
            } else {
                Bson::String(dims.iter().map(|d| format!("[1:{d}]")).collect())
            })
        }
        "array_to_string" => array_to_string(name, args),
        "string_to_array" => string_to_array(name, args),
        "array_position" | "array_positions" => array_position(name, args),
        "array_remove" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            let (a, e) = (arg(0), arg(1));
            if a == Bson::Null {
                return Ok(Bson::Null);
            }
            let items = need_array(name, &a)?;
            if dim_lengths(&a).len() > 1 {
                return Err(Error::FeatureNotSupported(
                    "removing elements from multidimensional arrays is not supported".into(),
                ));
            }
            Ok(Bson::Array(
                items.iter().filter(|x| !same(x, &e)).cloned().collect(),
            ))
        }
        "array_replace" => {
            if args.len() != 3 {
                return Err(wrong_args(name));
            }
            let (a, from, to) = (arg(0), arg(1), arg(2));
            if a == Bson::Null {
                return Ok(Bson::Null);
            }
            need_array(name, &a)?;
            // Replacement reaches every LEAF, so a multidimensional array is
            // rewritten in place rather than refused the way `array_remove`
            // is — removing would change the shape, replacing cannot.
            fn rewrite(v: &Bson, from: &Bson, to: &Bson) -> Bson {
                match v {
                    Bson::Array(items) => {
                        Bson::Array(items.iter().map(|i| rewrite(i, from, to)).collect())
                    }
                    other if same(other, from) => to.clone(),
                    other => other.clone(),
                }
            }
            Ok(rewrite(&a, &from, &to))
        }
        "array_cat" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            concat(name, &arg(0), &arg(1))
        }
        "array_append" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            append(name, &arg(0), &arg(1), false)
        }
        "array_prepend" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            append(name, &arg(1), &arg(0), true)
        }
        "array_fill" => array_fill(name, args),
        "trim_array" => {
            if args.len() != 2 {
                return Err(wrong_args(name));
            }
            if args.iter().any(|a| a == &Bson::Null) {
                return Ok(Bson::Null);
            }
            let items = need_array(name, &args[0])?;
            let n = as_i64(&args[1]).unwrap_or(-1);
            if n < 0 || n > items.len() as i64 {
                return Err(Error::Sqlstate(
                    "2202E",
                    format!(
                        "number of elements to trim must be between 0 and {}",
                        items.len()
                    ),
                ));
            }
            Ok(Bson::Array(items[..items.len() - n as usize].to_vec()))
        }
        _ => Err(wrong_args(name)),
    }
}

/// One array subscript as an integer. PostgreSQL types a subscript as `int4`
/// and refuses anything that will not coerce, rather than truncating it.
pub fn subscript_index(v: &Bson) -> Result<i64> {
    as_i64(v)
        .ok_or_else(|| Error::DatatypeMismatch("array subscript must have type integer".into()))
}

fn as_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        _ => crate::numeric_text(v).and_then(|t| t.parse::<f64>().ok().map(|f| f as i64)),
    }
}

/// `array_to_string(arr, sep [, null_string])`.
///
/// Without a `null_string` a NULL element is SKIPPED entirely — not rendered
/// as an empty string — so `array_to_string(ARRAY[1,NULL,3], ',')` is `1,3`
/// and not `1,,3`. A NULL `null_string` argument means the same as omitting
/// it. Multidimensional input is flattened in row-major order.
fn array_to_string(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() < 2 || args.len() > 3 {
        return Err(wrong_args(name));
    }
    let (a, sep) = (&args[0], &args[1]);
    if *a == Bson::Null || *sep == Bson::Null {
        return Ok(Bson::Null);
    }
    need_array(name, a)?;
    let null_text = args.get(2).filter(|v| **v != Bson::Null);
    let sep = crate::scalar::text(sep);
    let parts: Vec<String> = flatten(a)
        .iter()
        .filter_map(|v| match (v == &Bson::Null, null_text) {
            (true, None) => None,
            (true, Some(n)) => Some(crate::scalar::text(n)),
            (false, _) => Some(crate::scalar::text(v)),
        })
        .collect();
    Ok(Bson::String(parts.join(&sep)))
}

/// `string_to_array(text, sep [, null_string])`.
///
/// Three argument shapes PostgreSQL treats differently, all measured:
/// an EMPTY separator makes the whole string one element (`{abc}`), a NULL
/// separator splits into single characters (`{a,b,c}`), and an empty INPUT is
/// the empty array whatever the separator is.
fn string_to_array(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() < 2 || args.len() > 3 {
        return Err(wrong_args(name));
    }
    if args[0] == Bson::Null {
        return Ok(Bson::Null);
    }
    let s = crate::scalar::text(&args[0]);
    if s.is_empty() {
        return Ok(Bson::Array(Vec::new()));
    }
    let parts: Vec<String> = match &args[1] {
        Bson::Null => s.chars().map(|c| c.to_string()).collect(),
        sep => {
            let sep = crate::scalar::text(sep);
            if sep.is_empty() {
                vec![s]
            } else {
                s.split(&sep).map(str::to_string).collect()
            }
        }
    };
    let null_text = args
        .get(2)
        .filter(|v| **v != Bson::Null)
        .map(crate::scalar::text);
    Ok(Bson::Array(
        parts
            .into_iter()
            .map(|p| {
                if null_text.as_deref() == Some(p.as_str()) {
                    Bson::Null
                } else {
                    Bson::String(p)
                }
            })
            .collect(),
    ))
}

/// `array_position(arr, elem [, start])` and `array_positions(arr, elem)`.
///
/// Both refuse a multidimensional array by name, as PostgreSQL does — there is
/// no single subscript to report for a leaf of a 2-D array.
fn array_position(name: &str, args: &[Bson]) -> Result<Bson> {
    let plural = name == "array_positions";
    let allowed = if plural { 2..=2 } else { 2..=3 };
    if !allowed.contains(&args.len()) {
        return Err(wrong_args(name));
    }
    if args[0] == Bson::Null {
        return Ok(Bson::Null);
    }
    let items = need_array(name, &args[0])?;
    if dim_lengths(&args[0]).len() > 1 {
        return Err(Error::FeatureNotSupported(
            "searching for elements in multidimensional arrays is not supported".into(),
        ));
    }
    let wanted = &args[1];
    if plural {
        return Ok(Bson::Array(
            items
                .iter()
                .enumerate()
                .filter(|(_, v)| same(v, wanted))
                .map(|(i, _)| Bson::Int32(i32::try_from(i + 1).unwrap_or(i32::MAX)))
                .collect(),
        ));
    }
    // A NULL `start` is a NULL answer, not a search from 1.
    let start = match args.get(2) {
        Some(Bson::Null) => return Ok(Bson::Null),
        Some(v) => as_i64(v).unwrap_or(1).max(1),
        None => 1,
    };
    Ok(items
        .iter()
        .enumerate()
        .skip((start - 1) as usize)
        .find(|(_, v)| same(v, wanted))
        .map(|(i, _)| Bson::Int32(i32::try_from(i + 1).unwrap_or(i32::MAX)))
        .unwrap_or(Bson::Null))
}

/// `array_cat(a, b)` — and the `||` operator's array forms.
///
/// A NULL side is the OTHER side rather than a NULL result, which is what
/// makes `array_cat` usable as a fold over a nullable accumulator. Two arrays
/// of equal dimensionality join at the outer level; one of N dimensions joins
/// with one of N-1 by taking the shorter as a single row. Anything else is
/// PostgreSQL's own "cannot concatenate incompatible arrays".
fn concat(name: &str, a: &Bson, b: &Bson) -> Result<Bson> {
    match (a == &Bson::Null, b == &Bson::Null) {
        (true, true) => return Ok(Bson::Null),
        (true, false) => return need_array(name, b).map(|_| b.clone()),
        (false, true) => return need_array(name, a).map(|_| a.clone()),
        (false, false) => {}
    }
    let (ai, bi) = (need_array(name, a)?, need_array(name, b)?);
    // An empty array carries no shape to be incompatible with.
    if ai.is_empty() {
        return Ok(b.clone());
    }
    if bi.is_empty() {
        return Ok(a.clone());
    }
    let (da, db) = (dim_lengths(a), dim_lengths(b));
    let incompatible = || Error::DataException("cannot concatenate incompatible arrays".into());
    let out = match (da.len(), db.len()) {
        (x, y) if x == y => {
            if da[1..] != db[1..] {
                return Err(incompatible());
            }
            ai.iter().chain(bi.iter()).cloned().collect()
        }
        // `ARRAY[[1,2],[3,4]] || ARRAY[5,6]` — the 1-D side becomes one more
        // row, which needs its length to match the rows already there.
        (x, y) if x == y + 1 => {
            if da[1..] != db[..] {
                return Err(incompatible());
            }
            ai.iter()
                .cloned()
                .chain(std::iter::once(b.clone()))
                .collect()
        }
        (x, y) if y == x + 1 => {
            if db[1..] != da[..] {
                return Err(incompatible());
            }
            std::iter::once(a.clone())
                .chain(bi.iter().cloned())
                .collect()
        }
        _ => return Err(incompatible()),
    };
    Ok(Bson::Array(out))
}

/// `array_append(arr, elem)` / `array_prepend(elem, arr)`.
///
/// A NULL array is the one-element array, so a fold can start from NULL; a
/// NULL ELEMENT is appended as a NULL element rather than ignored. Only a
/// one-dimensional (or empty) array can take an element — PostgreSQL refuses
/// the 2-D case because a scalar is not a row.
fn append(name: &str, arr: &Bson, elem: &Bson, prepend: bool) -> Result<Bson> {
    if arr == &Bson::Null {
        return Ok(Bson::Array(vec![elem.clone()]));
    }
    let items = need_array(name, arr)?;
    if dim_lengths(arr).len() > 1 {
        return Err(Error::DataException(
            "argument must be empty or one-dimensional array".into(),
        ));
    }
    let mut out = items.clone();
    if prepend {
        out.insert(0, elem.clone());
    } else {
        out.push(elem.clone());
    }
    Ok(Bson::Array(out))
}

/// `array_fill(value, dims [, lbounds])`.
///
/// `lbounds` gives each dimension's lower bound (see `bounded`). A NULL
/// `dims` is PostgreSQL's own 22004, not a NULL result.
fn array_fill(name: &str, args: &[Bson]) -> Result<Bson> {
    if args.len() < 2 || args.len() > 3 {
        return Err(wrong_args(name));
    }
    let null_bound =
        || Error::NullValueNotAllowed("dimension array or low bound array cannot be null".into());
    if args[1] == Bson::Null {
        return Err(null_bound());
    }
    if let Some(lb) = args.get(2) {
        if *lb == Bson::Null {
            return Err(null_bound());
        }
        need_array(name, lb)?;
    }
    let lbs: Vec<i64> = match args.get(2) {
        Some(lb) => need_array(name, lb)?.iter().map(|b| as_i64(b).unwrap_or(1)).collect(),
        None => Vec::new(),
    };
    let dims: Vec<i64> = need_array(name, &args[1])?
        .iter()
        .map(|d| as_i64(d).unwrap_or(0))
        .collect();
    // Size the whole thing BEFORE allocating any of it.
    let mut total: i64 = 1;
    for d in &dims {
        total = total.saturating_mul((*d).max(0));
        check_array_size(total)?;
    }
    let mut out = args[0].clone();
    for d in dims.iter().rev() {
        out = Bson::Array(if *d > 0 {
            vec![out; *d as usize]
        } else {
            Vec::new()
        });
    }
    // `array_fill(v, ARRAY[])` — no dimensions at all — is still an array.
    Ok(match out {
        Bson::Array(_) => bounded(out, &lbs),
        scalar => Bson::Array(vec![scalar]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(n: i32) -> Bson {
        Bson::Int32(n)
    }

    fn arr(items: Vec<Bson>) -> Bson {
        Bson::Array(items)
    }

    fn two_by_two() -> Bson {
        arr(vec![arr(vec![i(1), i(2)]), arr(vec![i(3), i(4)])])
    }

    #[test]
    fn dimensions_are_read_off_the_first_element() {
        assert_eq!(dim_lengths(&arr(vec![i(1), i(2)])), vec![2]);
        assert_eq!(dim_lengths(&two_by_two()), vec![2, 2]);
        assert_eq!(dim_lengths(&arr(vec![])), Vec::<usize>::new());
    }

    #[test]
    fn empty_array_has_no_dimensions_but_zero_cardinality() {
        let empty = arr(vec![]);
        assert_eq!(
            call("array_ndims", std::slice::from_ref(&empty)).unwrap(),
            Bson::Null
        );
        assert_eq!(
            call("array_dims", std::slice::from_ref(&empty)).unwrap(),
            Bson::Null
        );
        assert_eq!(
            call("array_length", &[empty.clone(), i(1)]).unwrap(),
            Bson::Null
        );
        assert_eq!(call("cardinality", &[empty]).unwrap(), i(0));
    }

    #[test]
    fn cardinality_counts_every_leaf() {
        assert_eq!(call("cardinality", &[two_by_two()]).unwrap(), i(4));
        assert_eq!(call("array_length", &[two_by_two(), i(2)]).unwrap(), i(2));
        assert_eq!(
            call("array_dims", &[two_by_two()]).unwrap(),
            Bson::String("[1:2][1:2]".into())
        );
    }

    #[test]
    fn null_argument_rules_differ_per_function() {
        assert_eq!(
            call("array_remove", &[Bson::Null, i(1)]).unwrap(),
            Bson::Null
        );
        // array_cat and array_append take a NULL side as the empty one.
        assert_eq!(
            call("array_cat", &[Bson::Null, arr(vec![i(3)])]).unwrap(),
            arr(vec![i(3)])
        );
        assert_eq!(
            call("array_append", &[Bson::Null, i(2)]).unwrap(),
            arr(vec![i(2)])
        );
    }

    #[test]
    fn search_matches_null_to_null_but_containment_does_not() {
        let a = arr(vec![i(1), Bson::Null]);
        assert_eq!(
            call("array_position", &[a.clone(), Bson::Null]).unwrap(),
            i(2)
        );
        assert_eq!(
            call("array_remove", &[a.clone(), Bson::Null]).unwrap(),
            arr(vec![i(1)])
        );
        assert!(!contains(&a, &arr(vec![Bson::Null])));
        assert!(!overlaps(&a, &arr(vec![Bson::Null])));
    }

    #[test]
    fn containment_flattens_both_sides() {
        assert!(contains(&two_by_two(), &arr(vec![i(3)])));
        assert!(contains(&arr(vec![i(1), i(2)]), &arr(vec![])));
        assert!(overlaps(&arr(vec![i(1), i(2)]), &arr(vec![i(2), i(9)])));
        assert!(!overlaps(&arr(vec![i(1)]), &arr(vec![i(9)])));
    }

    #[test]
    fn a_short_subscript_list_is_null_not_the_inner_array() {
        let m = two_by_two();
        assert_eq!(element(&m, &[Some(1)], 2), Bson::Null);
        assert_eq!(element(&m, &[Some(1), Some(2)], 2), i(2));
        assert_eq!(element(&m, &[Some(9), Some(1)], 2), Bson::Null);
        assert_eq!(element(&m, &[Some(0)], 1), Bson::Null);
    }

    #[test]
    fn a_slice_out_of_range_is_empty_where_an_element_is_null() {
        let a = arr(vec![i(1), i(2), i(3)]);
        assert_eq!(element(&a, &[Some(9)], 1), Bson::Null);
        assert_eq!(slice(&a, &[(Some(5), Some(9))]).unwrap(), arr(vec![]));
        assert_eq!(slice(&a, &[(Some(3), Some(1))]).unwrap(), arr(vec![]));
        assert_eq!(
            slice(&a, &[(Some(0), Some(1))]).unwrap(),
            arr(vec![i(1)]),
            "a lower bound below 1 clamps rather than shifting"
        );
        assert_eq!(
            slice(&two_by_two(), &[(Some(1), Some(2)), (Some(1), Some(1))]).unwrap(),
            arr(vec![arr(vec![i(1)]), arr(vec![i(3)])])
        );
        assert_eq!(slice(&a, &[(None, Some(2))]), None);
    }

    #[test]
    fn array_to_string_skips_nulls_without_a_null_string() {
        let a = arr(vec![i(1), Bson::Null, i(3)]);
        assert_eq!(
            call("array_to_string", &[a.clone(), Bson::String(",".into())]).unwrap(),
            Bson::String("1,3".into())
        );
        assert_eq!(
            call(
                "array_to_string",
                &[a, Bson::String(",".into()), Bson::String("X".into())]
            )
            .unwrap(),
            Bson::String("1,X,3".into())
        );
    }

    #[test]
    fn string_to_array_separator_shapes() {
        let s = Bson::String("abc".into());
        assert_eq!(
            call("string_to_array", &[s.clone(), Bson::String("".into())]).unwrap(),
            arr(vec![Bson::String("abc".into())]),
            "an empty separator keeps the whole string"
        );
        assert_eq!(
            call("string_to_array", &[s, Bson::Null]).unwrap(),
            arr(vec![
                Bson::String("a".into()),
                Bson::String("b".into()),
                Bson::String("c".into())
            ]),
            "a NULL separator splits into characters"
        );
        assert_eq!(
            call(
                "string_to_array",
                &[Bson::String("".into()), Bson::String(",".into())]
            )
            .unwrap(),
            arr(vec![]),
        );
    }

    #[test]
    fn concatenation_joins_by_dimensionality() {
        assert_eq!(
            call(
                "array_cat",
                &[two_by_two(), arr(vec![arr(vec![i(5), i(6)])])]
            )
            .unwrap(),
            arr(vec![
                arr(vec![i(1), i(2)]),
                arr(vec![i(3), i(4)]),
                arr(vec![i(5), i(6)])
            ])
        );
        assert_eq!(
            call("array_cat", &[two_by_two(), arr(vec![i(5), i(6)])]).unwrap(),
            arr(vec![
                arr(vec![i(1), i(2)]),
                arr(vec![i(3), i(4)]),
                arr(vec![i(5), i(6)])
            ])
        );
        assert!(call(
            "array_cat",
            &[
                arr(vec![i(1), i(2)]),
                arr(vec![arr(vec![i(3), i(4), i(5)])])
            ]
        )
        .is_err());
    }

    #[test]
    fn multidimensional_removal_is_refused_but_replacement_is_not() {
        assert!(call("array_remove", &[two_by_two(), i(1)]).is_err());
        assert!(call("array_position", &[two_by_two(), i(1)]).is_err());
        assert_eq!(
            call("array_replace", &[two_by_two(), i(1), i(9)]).unwrap(),
            arr(vec![arr(vec![i(9), i(2)]), arr(vec![i(3), i(4)])])
        );
    }

    #[test]
    fn a_size_postgres_refuses_is_refused_before_it_is_allocated() {
        // The point is that this returns rather than trying to build it.
        let err = call("array_fill", &[i(1), arr(vec![Bson::Int64(1_000_000_000)])]);
        assert!(err.is_err());
        assert!(check_array_size(MAX_ARRAY_SIZE).is_ok());
        assert!(check_array_size(MAX_ARRAY_SIZE + 1).is_err());
        // The product across dimensions is what counts, not each one.
        assert!(call("array_fill", &[i(1), arr(vec![i(20_000), i(20_000)])]).is_err());
    }

    #[test]
    fn array_fill_refuses_a_lower_bound_it_cannot_represent() {
        assert_eq!(
            call("array_fill", &[i(0), arr(vec![i(2), i(2)])]).unwrap(),
            arr(vec![arr(vec![i(0), i(0)]), arr(vec![i(0), i(0)])])
        );
        assert_eq!(
            call("array_fill", &[i(1), arr(vec![i(0)])]).unwrap(),
            arr(vec![])
        );
        assert!(call("array_fill", &[i(7), arr(vec![i(2)]), arr(vec![i(3)])]).is_err());
        assert_eq!(
            call("array_fill", &[i(7), arr(vec![i(2)]), arr(vec![i(1)])]).unwrap(),
            arr(vec![i(7), i(7)])
        );
        assert!(call("array_fill", &[i(1), Bson::Null]).is_err());
    }
}

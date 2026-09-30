//! The range and multirange OPERATORS: `@>` `<@` `&&` `-|-` `<<` `>>` `&<`
//! `&>`, ordering, and `+` `*` `-`. Transcribed from PostgreSQL's
//! `rangetypes.c` / `multirangetypes.c`, which compare BOUNDS (a value, an
//! inclusivity, and whether it is a lower or an upper bound) rather than
//! values -- that is what makes `[1,5)` and `[5,8)` adjacent and `(1,5)`
//! start after `[1,5)`.
//!
//! Ranges and multiranges are carried as their canonical text, so which
//! operator runs is decided by the operands' STATIC types.

use std::cmp::Ordering;

use bson::Bson;

use crate::range::{self, Range};
use crate::{Error, Result};

#[derive(Clone)]
struct Bound {
    val: Option<String>,
    inclusive: bool,
    lower: bool,
}

fn lower(r: &Range) -> Bound {
    Bound {
        val: r.lower.clone(),
        inclusive: r.lower_inc,
        lower: true,
    }
}

fn upper(r: &Range) -> Bound {
    Bound {
        val: r.upper.clone(),
        inclusive: r.upper_inc,
        lower: false,
    }
}

/// `range_cmp_bounds`.
fn cmp(a: &Bound, b: &Bound, element: &str) -> Result<Ordering> {
    match (&a.val, &b.val) {
        (None, None) => Ok(if a.lower == b.lower {
            Ordering::Equal
        } else if a.lower {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (None, Some(_)) => Ok(if a.lower {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (Some(_), None) => Ok(if b.lower {
            Ordering::Greater
        } else {
            Ordering::Less
        }),
        (Some(x), Some(y)) => {
            let mut r = range::compare_bounds(x, y, element)?;
            if r == Ordering::Equal {
                if !a.inclusive && !b.inclusive {
                    r = if a.lower == b.lower {
                        Ordering::Equal
                    } else if a.lower {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                } else if !a.inclusive {
                    r = if a.lower {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                } else if !b.inclusive {
                    r = if b.lower {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
            }
            Ok(r)
        }
    }
}

fn make(lo: Bound, hi: Bound, type_name: &str) -> Result<Range> {
    let (element, discrete) = range::range_element(type_name)
        .ok_or_else(|| Error::Unsupported(format!("the {type_name} type")))?;
    let r = Range {
        empty: false,
        lower: lo.val,
        upper: hi.val,
        lower_inc: lo.inclusive,
        upper_inc: hi.inclusive,
    };
    range::canonicalise(r, &element, discrete, type_name)
}

fn contains_range(a: &Range, b: &Range, e: &str) -> Result<bool> {
    if b.empty {
        return Ok(true);
    }
    if a.empty {
        return Ok(false);
    }
    Ok(cmp(&lower(a), &lower(b), e)?.is_le() && cmp(&upper(a), &upper(b), e)?.is_ge())
}

fn contains_elem(r: &Range, v: &str, e: &str) -> Result<bool> {
    if r.empty {
        return Ok(false);
    }
    if let Some(l) = &r.lower {
        match range::compare_bounds(l, v, e)? {
            Ordering::Greater => return Ok(false),
            Ordering::Equal if !r.lower_inc => return Ok(false),
            _ => {}
        }
    }
    if let Some(u) = &r.upper {
        match range::compare_bounds(u, v, e)? {
            Ordering::Less => return Ok(false),
            Ordering::Equal if !r.upper_inc => return Ok(false),
            _ => {}
        }
    }
    Ok(true)
}

fn overlaps(a: &Range, b: &Range, e: &str) -> Result<bool> {
    if a.empty || b.empty {
        return Ok(false);
    }
    let (la, ua, lb, ub) = (lower(a), upper(a), lower(b), upper(b));
    Ok((cmp(&la, &lb, e)?.is_ge() && cmp(&la, &ub, e)?.is_le())
        || (cmp(&lb, &la, e)?.is_ge() && cmp(&lb, &ua, e)?.is_le()))
}

/// `bounds_adjacent`: an upper bound meeting a lower one at a value that
/// exactly one of them includes. (Discrete ranges are canonical, so
/// `[1,5)` / `[5,8)` meet at 5.)
fn bounds_adjacent(up: &Bound, lo: &Bound, e: &str) -> Result<bool> {
    match (&up.val, &lo.val) {
        (Some(x), Some(y)) => {
            Ok(range::compare_bounds(x, y, e)? == Ordering::Equal && up.inclusive != lo.inclusive)
        }
        _ => Ok(false),
    }
}

fn adjacent(a: &Range, b: &Range, e: &str) -> Result<bool> {
    if a.empty || b.empty {
        return Ok(false);
    }
    Ok(bounds_adjacent(&upper(a), &lower(b), e)? || bounds_adjacent(&upper(b), &lower(a), e)?)
}

fn range_eq(a: &Range, b: &Range, e: &str) -> Result<bool> {
    if a.empty || b.empty {
        return Ok(a.empty == b.empty);
    }
    Ok(cmp(&lower(a), &lower(b), e)? == Ordering::Equal
        && cmp(&upper(a), &upper(b), e)? == Ordering::Equal)
}

/// `range_cmp`: empty sorts first, then by lower bound, then upper.
fn range_order(a: &Range, b: &Range, e: &str) -> Result<Ordering> {
    match (a.empty, b.empty) {
        (true, true) => return Ok(Ordering::Equal),
        (true, false) => return Ok(Ordering::Less),
        (false, true) => return Ok(Ordering::Greater),
        _ => {}
    }
    match cmp(&lower(a), &lower(b), e)? {
        Ordering::Equal => cmp(&upper(a), &upper(b), e),
        other => Ok(other),
    }
}

fn union(a: &Range, b: &Range, t: &str, strict: bool) -> Result<Range> {
    let e = element(t)?;
    if a.empty {
        return Ok(b.clone());
    }
    if b.empty {
        return Ok(a.clone());
    }
    if strict && !overlaps(a, b, &e)? && !adjacent(a, b, &e)? {
        return Err(Error::DataException(
            "result of range union would not be contiguous".into(),
        ));
    }
    let lo = if cmp(&lower(a), &lower(b), &e)?.is_le() {
        lower(a)
    } else {
        lower(b)
    };
    let hi = if cmp(&upper(a), &upper(b), &e)?.is_ge() {
        upper(a)
    } else {
        upper(b)
    };
    make(lo, hi, t)
}

fn intersect(a: &Range, b: &Range, t: &str) -> Result<Range> {
    let e = element(t)?;
    if a.empty || b.empty || !overlaps(a, b, &e)? {
        return Ok(Range::empty());
    }
    let lo = if cmp(&lower(a), &lower(b), &e)?.is_ge() {
        lower(a)
    } else {
        lower(b)
    };
    let hi = if cmp(&upper(a), &upper(b), &e)?.is_le() {
        upper(a)
    } else {
        upper(b)
    };
    make(lo, hi, t)
}

/// `a - b` as the pieces it leaves (none, one, or two).
fn minus_pieces(a: &Range, b: &Range, t: &str) -> Result<Vec<Range>> {
    let e = element(t)?;
    if a.empty {
        return Ok(Vec::new());
    }
    if b.empty || !overlaps(a, b, &e)? {
        return Ok(vec![a.clone()]);
    }
    let (la, ua, lb, ub) = (lower(a), upper(a), lower(b), upper(b));
    let mut out = Vec::new();
    if cmp(&la, &lb, &e)?.is_lt() {
        let mut hi = lb.clone();
        hi.inclusive = !hi.inclusive;
        hi.lower = false;
        let r = make(la.clone(), hi, t)?;
        if !r.empty {
            out.push(r);
        }
    }
    if cmp(&ua, &ub, &e)?.is_gt() {
        let mut lo = ub.clone();
        lo.inclusive = !lo.inclusive;
        lo.lower = true;
        let r = make(lo, ua.clone(), t)?;
        if !r.empty {
            out.push(r);
        }
    }
    Ok(out)
}

fn element(t: &str) -> Result<String> {
    range::range_element(t)
        .map(|(e, _)| e)
        .ok_or_else(|| Error::Unsupported(format!("the {t} type")))
}

/// Either side of a binary operator: a range, a multirange (its member type
/// and members), or an element value.
enum Side {
    Range(Range, String),
    Multi(Vec<Range>, String),
    Elem(Bson),
}

fn side(v: &Bson, ty: &str) -> Result<Side> {
    let text = crate::render_value_text(v);
    if range::is_range_type(ty) {
        return Ok(Side::Range(range::from_text(&text, ty)?, ty.to_string()));
    }
    if range::is_multirange_type(ty) {
        let member = range::multirange_member(ty).unwrap_or_default();
        return Ok(Side::Multi(range::multirange_from_text(&text, ty)?, member));
    }
    Ok(Side::Elem(v.clone()))
}

fn as_multi(s: &Side) -> Option<(Vec<Range>, String)> {
    match s {
        Side::Range(r, t) => Some((vec![r.clone()], t.clone())),
        Side::Multi(m, t) => Some((m.clone(), t.clone())),
        Side::Elem(_) => None,
    }
}

/// Evaluate `op` when either operand is (statically) a range or multirange.
/// `None` when this is not a range operator.
pub fn binary(op: &str, lhs: &Bson, rhs: &Bson, lt: &str, rt: &str) -> Option<Result<Bson>> {
    if !(range::is_range_type(lt)
        || range::is_multirange_type(lt)
        || range::is_range_type(rt)
        || range::is_multirange_type(rt))
    {
        return None;
    }
    if !matches!(
        op,
        "@>" | "<@"
            | "&&"
            | "-|-"
            | "<<"
            | ">>"
            | "&<"
            | "&>"
            | "="
            | "<>"
            | "!="
            | "<"
            | "<="
            | ">"
            | ">="
            | "+"
            | "*"
            | "-"
    ) {
        return None;
    }
    // The set operators and comparisons exist only between two of the SAME
    // kind; containment and overlap also mix a range with a multirange.
    let multi = |t: &str| range::is_multirange_type(t);
    if matches!(
        op,
        "+" | "*" | "-" | "=" | "<>" | "!=" | "<" | "<=" | ">" | ">="
    ) && multi(lt) != multi(rt)
        && (range::is_range_type(lt) || multi(lt))
        && (range::is_range_type(rt) || multi(rt))
    {
        return Some(Err(Error::UndefinedFunction(format!(
            "operator does not exist: {lt} {op} {rt}"
        ))));
    }
    if *lhs == Bson::Null || *rhs == Bson::Null {
        return Some(Ok(Bson::Null));
    }
    Some(eval(op, lhs, rhs, lt, rt))
}

fn eval(op: &str, lhs: &Bson, rhs: &Bson, lt: &str, rt: &str) -> Result<Bson> {
    let (a, b) = (side(lhs, lt)?, side(rhs, rt)?);
    // Element containment.
    match (&a, &b, op) {
        (Side::Range(r, t), Side::Elem(v), "@>") | (Side::Elem(v), Side::Range(r, t), "<@") => {
            let e = element(t)?;
            return Ok(Bson::Boolean(contains_elem(
                r,
                &crate::render_value_text(v),
                &e,
            )?));
        }
        (Side::Multi(m, t), Side::Elem(v), "@>") | (Side::Elem(v), Side::Multi(m, t), "<@") => {
            let e = element(t)?;
            let text = crate::render_value_text(v);
            let mut any = false;
            for r in m {
                any |= contains_elem(r, &text, &e)?;
            }
            return Ok(Bson::Boolean(any));
        }
        _ => {}
    }
    // Range with range.
    if let (Side::Range(x, t), Side::Range(y, _)) = (&a, &b) {
        let e = element(t)?;
        let bool_ = |v: bool| Ok(Bson::Boolean(v));
        return match op {
            "@>" => bool_(contains_range(x, y, &e)?),
            "<@" => bool_(contains_range(y, x, &e)?),
            "&&" => bool_(overlaps(x, y, &e)?),
            "-|-" => bool_(adjacent(x, y, &e)?),
            "<<" => bool_(!x.empty && !y.empty && cmp(&upper(x), &lower(y), &e)?.is_lt()),
            ">>" => bool_(!x.empty && !y.empty && cmp(&lower(x), &upper(y), &e)?.is_gt()),
            "&<" => bool_(!x.empty && !y.empty && cmp(&upper(x), &upper(y), &e)?.is_le()),
            "&>" => bool_(!x.empty && !y.empty && cmp(&lower(x), &lower(y), &e)?.is_ge()),
            "=" => bool_(range_eq(x, y, &e)?),
            "<>" | "!=" => bool_(!range_eq(x, y, &e)?),
            "<" => bool_(range_order(x, y, &e)?.is_lt()),
            "<=" => bool_(range_order(x, y, &e)?.is_le()),
            ">" => bool_(range_order(x, y, &e)?.is_gt()),
            ">=" => bool_(range_order(x, y, &e)?.is_ge()),
            "+" => Ok(Bson::String(range::render(&union(x, y, t, true)?))),
            "*" => Ok(Bson::String(range::render(&intersect(x, y, t)?))),
            "-" => {
                let pieces = minus_pieces(x, y, t)?;
                match pieces.as_slice() {
                    [] => Ok(Bson::String("empty".into())),
                    [one] => Ok(Bson::String(range::render(one))),
                    _ => Err(Error::DataException(
                        "result of range difference would not be contiguous".into(),
                    )),
                }
            }
            _ => Err(Error::Unsupported(format!("range operator {op}"))),
        };
    }
    // Anything with a multirange: work over member lists.
    let (Some((ma, t)), Some((mb, _))) = (as_multi(&a), as_multi(&b)) else {
        return Err(Error::Unsupported(format!(
            "operator {op} on these operands"
        )));
    };
    let e = element(&t)?;
    let norm = |v: Vec<Range>| range::normalise_multirange(v, &t);
    let render =
        |v: Vec<Range>| -> Result<Bson> { Ok(Bson::String(range::render_multirange(&norm(v)?))) };
    match op {
        "+" => render(ma.into_iter().chain(mb).collect()),
        "*" => {
            let mut out = Vec::new();
            for x in &ma {
                for y in &mb {
                    let r = intersect(x, y, &t)?;
                    if !r.empty {
                        out.push(r);
                    }
                }
            }
            render(out)
        }
        "-" => {
            let mut cur = ma;
            for y in &mb {
                let mut next = Vec::new();
                for x in &cur {
                    next.extend(minus_pieces(x, y, &t)?);
                }
                cur = next;
            }
            render(cur)
        }
        "@>" | "<@" => {
            let (outer, inner) = if op == "@>" { (&ma, &mb) } else { (&mb, &ma) };
            let mut all = true;
            for y in inner.iter().filter(|r| !r.empty) {
                let mut some = false;
                for x in outer.iter() {
                    some |= contains_range(x, y, &e)?;
                }
                all &= some;
            }
            Ok(Bson::Boolean(all))
        }
        "&&" => {
            let mut any = false;
            for x in &ma {
                for y in &mb {
                    any |= overlaps(x, y, &e)?;
                }
            }
            Ok(Bson::Boolean(any))
        }
        "=" | "<>" | "!=" => {
            let (x, y) = (norm(ma)?, norm(mb)?);
            let same = x.len() == y.len()
                && x.iter().zip(&y).try_fold(true, |acc, (p, q)| {
                    Ok::<_, Error>(acc && range_eq(p, q, &e)?)
                })?;
            Ok(Bson::Boolean(if op == "=" { same } else { !same }))
        }
        _ => Err(Error::Unsupported(format!("multirange operator {op}"))),
    }
}

/// A range operator's result type, for the planner.
pub fn result_type(op: &str, lt: &str, rt: &str) -> Option<String> {
    let range_side = |t: &str| range::is_range_type(t) || range::is_multirange_type(t);
    if !(range_side(lt) || range_side(rt)) {
        return None;
    }
    Some(match op {
        "+" | "*" | "-" => {
            if range::is_multirange_type(lt) {
                lt.to_string()
            } else if range::is_multirange_type(rt) {
                rt.to_string()
            } else {
                lt.to_string()
            }
        }
        _ => "bool".to_string(),
    })
}

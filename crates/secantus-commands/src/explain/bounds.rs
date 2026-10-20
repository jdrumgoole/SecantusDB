//! Index bounds, built and printed the way mongod's planner does it.
//!
//! One predicate on an indexed field becomes an ordered list of intervals
//! over the index key. Every rule here was read off `mongod` 8.2.11 with
//! `tools/probes/explain_commands.py`; nothing is taken from the storage
//! layer's own index router, which answers a different question (how THIS
//! server fetches the documents).

use bson::{Bson, Document};
use secantus_core::sortkey::encode_value;
use std::cmp::Ordering;

/// One interval of index keys.
#[derive(Clone, Debug)]
pub(crate) struct Interval {
    pub lo: Bson,
    pub lo_incl: bool,
    pub hi: Bson,
    pub hi_incl: bool,
}

/// How much of the predicate the bounds already decide.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Tight {
    /// The bounds are the predicate; nothing is rechecked.
    Exact,
    /// Rechecked against the index key, on the IXSCAN stage.
    IndexFilter,
    /// Rechecked against the document, on the FETCH stage.
    FetchFilter,
}

/// What `leaf_bounds` needs to know about the index and the query.
#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    pub multikey: bool,
    pub sparse: bool,
    /// The query's collation is not the index's: a string cannot be bounded.
    pub collated: bool,
    /// The index holds plain string keys (it has no collation of its own).
    pub plain_strings: bool,
    /// `count` reads no documents, so an equality to null needs no recheck.
    pub keys_only: bool,
}

/// The byte-sortable key of a value, for ordering interval ends.
pub(crate) fn sort_key(v: &Bson) -> Vec<u8> {
    // An index holds `undefined` for an empty array, between MinKey and
    // null; the sort-key encoder has no key for it.
    if matches!(v, Bson::Undefined) {
        let mut key = encode_value(&Bson::MinKey, None).unwrap_or_default();
        key.push(0x80);
        return key;
    }
    encode_value(v, None).unwrap_or_default()
}

pub(crate) fn cmp(a: &Bson, b: &Bson) -> Ordering {
    sort_key(a).cmp(&sort_key(b))
}

impl Interval {
    pub fn point(v: Bson) -> Self {
        Interval {
            lo: v.clone(),
            lo_incl: true,
            hi: v,
            hi_incl: true,
        }
    }

    pub fn new(lo: Bson, lo_incl: bool, hi: Bson, hi_incl: bool) -> Self {
        Interval {
            lo,
            lo_incl,
            hi,
            hi_incl,
        }
    }

    pub fn all() -> Self {
        Interval::new(Bson::MinKey, true, Bson::MaxKey, true)
    }

    pub fn is_all(&self) -> bool {
        self.lo == Bson::MinKey && self.hi == Bson::MaxKey && self.lo_incl && self.hi_incl
    }

    pub fn is_point(&self) -> bool {
        self.lo_incl && self.hi_incl && cmp(&self.lo, &self.hi) == Ordering::Equal
    }

    fn is_empty(&self) -> bool {
        match cmp(&self.lo, &self.hi) {
            Ordering::Greater => true,
            Ordering::Equal => !(self.lo_incl && self.hi_incl),
            Ordering::Less => false,
        }
    }

    fn intersect(&self, other: &Interval) -> Option<Interval> {
        let (lo, lo_incl) = match cmp(&self.lo, &other.lo) {
            Ordering::Greater => (self.lo.clone(), self.lo_incl),
            Ordering::Less => (other.lo.clone(), other.lo_incl),
            Ordering::Equal => (self.lo.clone(), self.lo_incl && other.lo_incl),
        };
        let (hi, hi_incl) = match cmp(&self.hi, &other.hi) {
            Ordering::Less => (self.hi.clone(), self.hi_incl),
            Ordering::Greater => (other.hi.clone(), other.hi_incl),
            Ordering::Equal => (self.hi.clone(), self.hi_incl && other.hi_incl),
        };
        let out = Interval::new(lo, lo_incl, hi, hi_incl);
        (!out.is_empty()).then_some(out)
    }
}

/// Every pairwise overlap of two ascending lists.
pub(crate) fn intersect(a: &[Interval], b: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    for x in a {
        for y in b {
            if let Some(i) = x.intersect(y) {
                out.push(i);
            }
        }
    }
    out
}

/// Sort ascending and merge what overlaps.
pub(crate) fn union(mut list: Vec<Interval>) -> Vec<Interval> {
    list.retain(|i| !i.is_empty());
    list.sort_by(|a, b| cmp(&a.lo, &b.lo).then(b.lo_incl.cmp(&a.lo_incl)));
    let mut out: Vec<Interval> = Vec::new();
    for i in list {
        if let Some(last) = out.last_mut() {
            let touch = match cmp(&i.lo, &last.hi) {
                Ordering::Less => true,
                Ordering::Equal => i.lo_incl && last.hi_incl,
                Ordering::Greater => false,
            };
            if touch {
                match cmp(&i.hi, &last.hi) {
                    Ordering::Greater => {
                        last.hi = i.hi;
                        last.hi_incl = i.hi_incl;
                    }
                    Ordering::Equal => last.hi_incl |= i.hi_incl,
                    Ordering::Less => {}
                }
                continue;
            }
        }
        out.push(i);
    }
    out
}

/// Everything between MinKey and MaxKey that the list leaves out.
fn complement(list: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    let (mut lo, mut lo_incl) = (Bson::MinKey, true);
    for i in list {
        let gap = Interval::new(lo, lo_incl, i.lo.clone(), !i.lo_incl);
        if !gap.is_empty() {
            out.push(gap);
        }
        lo = i.hi.clone();
        lo_incl = !i.hi_incl;
    }
    let last = Interval::new(lo, lo_incl, Bson::MaxKey, true);
    if !last.is_empty() {
        out.push(last);
    }
    out
}

fn has_string(v: &Bson) -> bool {
    match v {
        Bson::String(_) | Bson::Symbol(_) => true,
        Bson::Document(d) => d.values().any(has_string),
        Bson::Array(a) => a.iter().any(has_string),
        _ => false,
    }
}

fn eq_bounds(v: &Bson, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    if ctx.collated && has_string(v) {
        return None;
    }
    match v {
        Bson::Null => {
            let tight = if ctx.keys_only && !ctx.multikey {
                Tight::Exact
            } else {
                Tight::FetchFilter
            };
            (!ctx.sparse).then(|| (vec![Interval::point(Bson::Null)], tight))
        }
        Bson::Undefined => None,
        Bson::Array(items) => {
            let first = items.first().cloned().unwrap_or(Bson::Undefined);
            let list = union(vec![Interval::point(first), Interval::point(v.clone())]);
            Some((list, Tight::FetchFilter))
        }
        Bson::RegularExpression(re) => regex_bounds(&re.pattern, &re.options, ctx),
        _ => Some((vec![Interval::point(v.clone())], Tight::Exact)),
    }
}

/// The lowest and highest value of the operand's type bracket.
fn bracket(v: &Bson) -> Option<(Bson, bool, Bson, bool)> {
    Some(match v {
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => (
            Bson::Double(f64::NEG_INFINITY),
            true,
            Bson::Double(f64::INFINITY),
            true,
        ),
        Bson::String(_) | Bson::Symbol(_) => (
            Bson::String(String::new()),
            true,
            Bson::Document(Document::new()),
            false,
        ),
        Bson::Document(_) => (
            Bson::Document(Document::new()),
            true,
            Bson::Array(vec![]),
            false,
        ),
        Bson::Boolean(_) => (Bson::Boolean(false), true, Bson::Boolean(true), true),
        Bson::DateTime(_) => (
            Bson::DateTime(bson::DateTime::from_millis(i64::MIN)),
            true,
            Bson::DateTime(bson::DateTime::from_millis(i64::MAX)),
            true,
        ),
        Bson::ObjectId(_) => (
            Bson::ObjectId(bson::oid::ObjectId::from_bytes([0; 12])),
            true,
            Bson::ObjectId(bson::oid::ObjectId::from_bytes([0xff; 12])),
            true,
        ),
        _ => return None,
    })
}

fn range_bounds(op: &str, v: &Bson, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    if ctx.collated && has_string(v) {
        return None;
    }
    let lower = op == "$gt" || op == "$gte";
    let incl = op == "$gte" || op == "$lte";
    let one = |i: Interval| {
        let list = if i.is_empty() { vec![] } else { vec![i] };
        Some((list, Tight::Exact))
    };
    match v {
        Bson::Null => {
            if ctx.sparse {
                return None;
            }
            if incl {
                Some((vec![Interval::point(Bson::Null)], Tight::FetchFilter))
            } else {
                Some((vec![], Tight::Exact))
            }
        }
        Bson::MinKey | Bson::MaxKey => one(if lower {
            Interval::new(v.clone(), incl, Bson::MaxKey, true)
        } else {
            Interval::new(Bson::MinKey, true, v.clone(), incl)
        }),
        Bson::Double(d) if d.is_nan() => one(Interval::new(v.clone(), incl, v.clone(), incl)),
        // A comparison to an array (mongod 8.2.11, 2026-10-20, multikey or
        // not): from the first element up for `$gt` / `$gte`, from MinKey to
        // the array itself for `$lt` / `$lte`, both ends closed and the
        // predicate rechecked; an empty array has no first element and
        // bounds nothing from below.
        Bson::Array(items) => {
            let interval = if lower {
                match items.first() {
                    Some(first) => Interval::new(first.clone(), true, Bson::MaxKey, true),
                    None => Interval::all(),
                }
            } else {
                Interval::new(Bson::MinKey, true, v.clone(), true)
            };
            Some((vec![interval], Tight::FetchFilter))
        }
        _ => {
            let (lo, lo_incl, hi, hi_incl) = bracket(v)?;
            one(if lower {
                Interval::new(v.clone(), incl, hi, hi_incl)
            } else {
                Interval::new(lo, lo_incl, v.clone(), incl)
            })
        }
    }
}

/// The literal prefix of an anchored regex, and whether the regex is nothing
/// but that prefix (optionally followed by `.*`).
fn regex_prefix(pattern: &str, options: &str) -> (String, bool) {
    if !options.is_empty() {
        return (String::new(), false);
    }
    let Some(rest) = pattern.strip_prefix('^') else {
        return (String::new(), false);
    };
    let chars: Vec<char> = rest.chars().collect();
    let mut prefix = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let literal = if c == '\\' {
            match chars.get(i + 1) {
                Some(n) if !n.is_alphanumeric() => {
                    i += 1;
                    *n
                }
                _ => break,
            }
        } else if ".*+?|()[]{}^$".contains(c) {
            break;
        } else {
            c
        };
        // A quantifier makes the character before it optional.
        if matches!(chars.get(i + 1), Some('*' | '?' | '{')) {
            break;
        }
        prefix.push(literal);
        i += 1;
    }
    let tail: String = chars[i..].iter().collect();
    let exact = !prefix.is_empty() && (tail.is_empty() || tail == ".*");
    (prefix, exact)
}

fn regex_bounds(pattern: &str, options: &str, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    // A regex matches the string as it is stored, whatever the query's
    // collation, so an index of plain strings still bounds it. An index of
    // collation keys holds no string as stored: mongod does not read it for
    // a regex at all, even under its own collation (measured 8.2.11: with
    // `w_1` and a `w_ci` of the query's collation, `w_ci` is not a
    // candidate for `{w: /^a/}`).
    if !ctx.plain_strings {
        return None;
    }
    let (prefix, exact) = regex_prefix(pattern, options);
    let re = Bson::RegularExpression(bson::Regex {
        pattern: pattern.to_string(),
        options: options.to_string(),
    });
    let strings = if prefix.is_empty() {
        Interval::new(
            Bson::String(String::new()),
            true,
            Bson::Document(Document::new()),
            false,
        )
    } else {
        let mut next: Vec<char> = prefix.chars().collect();
        if let Some(last) = next.last_mut() {
            *last = char::from_u32(*last as u32 + 1).unwrap_or(*last);
        }
        Interval::new(
            Bson::String(prefix),
            true,
            Bson::String(next.into_iter().collect()),
            false,
        )
    };
    let tight = if exact {
        Tight::Exact
    } else if ctx.multikey {
        Tight::FetchFilter
    } else {
        Tight::IndexFilter
    };
    Some((vec![strings, Interval::point(re)], tight))
}

fn numbers() -> Interval {
    Interval::new(
        Bson::Double(f64::NAN),
        true,
        Bson::Double(f64::INFINITY),
        true,
    )
}

fn type_bounds(v: &Bson, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    let items: Vec<Bson> = match v {
        Bson::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let numeric = [1, 16, 18, 19];
    // The alias `number` stands for all four numeric codes.
    let mut codes: Vec<i64> = Vec::new();
    for item in &items {
        match item {
            Bson::String(s) if s == "number" => codes.extend(numeric),
            other => codes.push(crate::util::as_i64(other)?),
        }
    }
    let mut list = Vec::new();
    let mut tight = Tight::Exact;
    let inexact = if ctx.multikey {
        Tight::FetchFilter
    } else {
        Tight::IndexFilter
    };
    for code in &codes {
        match code {
            1 | 16 | 18 | 19 => {
                list.push(numbers());
                if !numeric.iter().all(|n| codes.contains(n)) {
                    tight = tight.max(inexact);
                }
            }
            2 => {
                if ctx.collated {
                    return None;
                }
                list.push(Interval::new(
                    Bson::String(String::new()),
                    true,
                    Bson::Document(Document::new()),
                    false,
                ));
                tight = tight.max(inexact);
            }
            10 => {
                if ctx.sparse {
                    return None;
                }
                list.push(Interval::point(Bson::Null));
                tight = Tight::FetchFilter;
            }
            4 => return Some((vec![Interval::all()], Tight::FetchFilter)),
            _ => return None,
        }
    }
    Some((union(list), tight))
}

/// The bounds one canonical predicate (`{$op: operand}`) puts on a field.
/// `None` when the predicate cannot use an index at all.
/// The bounds of a twin `$expr` derives (`$_internalExprEq` and its range
/// siblings). `$expr` compares across types with no bracketing, so a range
/// runs to MinKey or MaxKey; an index key of null also stands for a missing
/// field, which `$expr` orders BELOW null, so the shapes that would take a
/// missing field in or out are rechecked (measured 8.2.11, 2026-10-16). Not
/// over a multikey field, where `$expr` does not look inside an array.
fn internal_expr_bounds(op: &str, v: &Bson, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    if ctx.multikey
        || matches!(v, Bson::Array(_))
        || (has_string(v) && (ctx.collated || !ctx.plain_strings))
    {
        return None;
    }
    let null = matches!(v, Bson::Null);
    let tight = |inexact: bool| {
        if inexact {
            Tight::FetchFilter
        } else {
            Tight::Exact
        }
    };
    let (list, inexact) = match op {
        "$_internalExprEq" => (vec![Interval::point(v.clone())], null),
        "$_internalExprLt" if matches!(v, Bson::MinKey) => (vec![], false),
        "$_internalExprLt" => (
            vec![Interval::new(Bson::MinKey, true, v.clone(), null)],
            null,
        ),
        "$_internalExprLte" => (
            vec![Interval::new(Bson::MinKey, true, v.clone(), true)],
            false,
        ),
        "$_internalExprGt" if matches!(v, Bson::MaxKey) => (vec![], false),
        "$_internalExprGt" => (
            vec![Interval::new(v.clone(), false, Bson::MaxKey, true)],
            false,
        ),
        "$_internalExprGte" => (
            vec![Interval::new(v.clone(), true, Bson::MaxKey, true)],
            null,
        ),
        _ => return None,
    };
    Some((list, tight(inexact)))
}

pub(crate) fn leaf_bounds(pred: &Document, ctx: Ctx) -> Option<(Vec<Interval>, Tight)> {
    let (op, v) = pred.iter().next()?;
    if op.starts_with("$_internalExpr") {
        return internal_expr_bounds(op, v, ctx);
    }
    match op.as_str() {
        // `$eq` of a regex is an equality to the regex value: one point
        // (an `$in`'s regex is a pattern; mongod 8.2.11, 2026-10-20).
        "$eq" if matches!(v, Bson::RegularExpression(_)) => {
            Some((vec![Interval::point(v.clone())], Tight::Exact))
        }
        "$eq" => eq_bounds(v, ctx),
        "$gt" | "$gte" | "$lt" | "$lte" => range_bounds(op, v, ctx),
        "$in" => {
            let items = v.as_array()?;
            let mut list = Vec::new();
            let mut tight = Tight::Exact;
            for item in items {
                let (part, t) = eq_bounds(item, ctx)?;
                list.extend(part);
                tight = tight.max(t);
            }
            Some((union(list), tight))
        }
        "$not" => {
            let inner = v.as_document()?;
            let inner_op = inner.keys().next()?.as_str();
            if ctx.sparse {
                return None;
            }
            if inner_op == "$exists" {
                return Some((vec![Interval::point(Bson::Null)], Tight::FetchFilter));
            }
            if !matches!(inner_op, "$eq" | "$in" | "$gt" | "$gte" | "$lt" | "$lte") {
                return None;
            }
            let plain = Ctx {
                multikey: false,
                ..ctx
            };
            let (list, t) = leaf_bounds(inner, plain)?;
            let arrays = match inner.values().next() {
                Some(Bson::Array(a)) if inner_op == "$in" => {
                    a.iter().any(|x| matches!(x, Bson::Array(_)))
                }
                Some(Bson::Array(_)) => true,
                _ => false,
            };
            if arrays || t == Tight::IndexFilter {
                return None;
            }
            let tight = if ctx.multikey {
                Tight::FetchFilter
            } else {
                Tight::Exact
            };
            Some((complement(&list), tight))
        }
        "$exists" => {
            let tight = if ctx.sparse && !ctx.multikey {
                Tight::Exact
            } else {
                Tight::FetchFilter
            };
            Some((vec![Interval::all()], tight))
        }
        "$regex" => {
            let (pattern, mut options) = match v {
                Bson::String(s) => (s.clone(), String::new()),
                Bson::RegularExpression(re) => (re.pattern.clone(), re.options.clone()),
                _ => return None,
            };
            if let Ok(o) = pred.get_str("$options") {
                options = o.to_string();
            }
            regex_bounds(&pattern, &options, ctx)
        }
        "$type" => type_bounds(v, ctx),
        "$mod" => Some((
            vec![numbers()],
            if ctx.multikey {
                Tight::FetchFilter
            } else {
                Tight::IndexFilter
            },
        )),
        "$elemMatch" => {
            // The value form: every inner operator bounds the same element,
            // so they intersect even on a multikey index.
            let inner = v.as_document()?;
            if inner.keys().any(|k| !k.starts_with('$')) {
                return None;
            }
            let plain = Ctx {
                multikey: false,
                ..ctx
            };
            let mut list: Option<Vec<Interval>> = None;
            for (k, val) in inner {
                let mut one = Document::new();
                one.insert(k.clone(), val.clone());
                if let Some((part, _)) = leaf_bounds(&one, plain) {
                    list = Some(match list {
                        Some(have) => intersect(&have, &part),
                        None => part,
                    });
                }
            }
            list.map(|l| (l, Tight::FetchFilter))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- printing

fn render_f64(d: f64) -> String {
    if d.is_nan() {
        "nan".into()
    } else if d.is_infinite() {
        if d > 0.0 { "inf" } else { "-inf" }.into()
    } else if d == d.trunc() && d.abs() < 1e15 {
        format!("{d:.1}")
    } else {
        format!("{d}")
    }
}

fn render_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A value as mongod prints it inside a bound.
pub(crate) fn render(v: &Bson) -> String {
    match v {
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(d) => render_f64(*d),
        Bson::Decimal128(d) => d.to_string(),
        Bson::String(s) => render_string(s),
        Bson::Symbol(s) => render_string(s),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Boolean(b) => b.to_string(),
        Bson::MinKey => "MinKey".into(),
        Bson::MaxKey => "MaxKey".into(),
        Bson::DateTime(d) => format!("new Date({})", d.timestamp_millis()),
        Bson::ObjectId(o) => format!("ObjectId('{}')", o.to_hex()),
        Bson::RegularExpression(re) => format!("/{}/{}", re.pattern, re.options),
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        Bson::Document(d) if d.is_empty() => "{}".into(),
        Bson::Document(d) => {
            let parts: Vec<String> = d
                .iter()
                .map(|(k, v)| format!("{k}: {}", render(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Bson::Array(a) if a.is_empty() => "[]".into(),
        Bson::Array(a) => {
            let parts: Vec<String> = a.iter().map(render).collect();
            format!("[ {} ]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

/// A key pattern as mongod prints it in one line: `{ a: 1, b: -1 }`.
pub(crate) fn render_key_pattern(key: &Document) -> String {
    render(&Bson::Document(key.clone()))
}

/// The bounds of one field as `explain` shows them. `reversed` is true when
/// the scan meets this field's keys from high to low: a descending field
/// scanned forward, or an ascending one scanned backward.
pub(crate) fn render_bounds(list: &[Interval], reversed: bool) -> Vec<Bson> {
    let one = |i: &Interval| {
        if reversed {
            format!(
                "{}{}, {}{}",
                if i.hi_incl { '[' } else { '(' },
                render(&i.hi),
                render(&i.lo),
                if i.lo_incl { ']' } else { ')' }
            )
        } else {
            format!(
                "{}{}, {}{}",
                if i.lo_incl { '[' } else { '(' },
                render(&i.lo),
                render(&i.hi),
                if i.hi_incl { ']' } else { ')' }
            )
        }
    };
    let mut out: Vec<Bson> = list.iter().map(|i| Bson::String(one(i))).collect();
    if reversed {
        out.reverse();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn shown(pred: Document, ctx: Ctx) -> Option<(Vec<String>, Tight)> {
        leaf_bounds(&pred, ctx).map(|(l, t)| {
            let texts = render_bounds(&l, false)
                .into_iter()
                .map(|b| b.as_str().unwrap().to_string())
                .collect();
            (texts, t)
        })
    }

    #[test]
    fn comparisons_stay_inside_the_type_bracket() {
        let c = Ctx::default();
        assert_eq!(shown(doc! {"$gt": 2}, c).unwrap().0, ["(2, inf]"]);
        assert_eq!(shown(doc! {"$lte": 2}, c).unwrap().0, ["[-inf, 2]"]);
        assert_eq!(shown(doc! {"$gt": "s"}, c).unwrap().0, ["(\"s\", {})"]);
        assert_eq!(shown(doc! {"$lt": "s"}, c).unwrap().0, ["[\"\", \"s\")"]);
        assert_eq!(shown(doc! {"$gt": false}, c).unwrap().0, ["(false, true]"]);
        assert!(shown(doc! {"$gt": f64::INFINITY}, c).unwrap().0.is_empty());
        assert!(shown(doc! {"$gt": Bson::Null}, c).unwrap().0.is_empty());
    }

    #[test]
    fn negation_is_the_complement() {
        let c = Ctx::default();
        let ne = shown(doc! {"$not": {"$eq": 2}}, c).unwrap();
        assert_eq!(ne.0, ["[MinKey, 2)", "(2, MaxKey]"]);
        assert_eq!(ne.1, Tight::Exact);
        let nin = shown(doc! {"$not": {"$in": [1, 3]}}, c).unwrap();
        assert_eq!(nin.0, ["[MinKey, 1)", "(1, 3)", "(3, MaxKey]"]);
        let not_gt = shown(doc! {"$not": {"$gt": 2}}, c).unwrap();
        assert_eq!(not_gt.0, ["[MinKey, 2]", "(inf, MaxKey]"]);
        let sparse = Ctx { sparse: true, ..c };
        assert!(shown(doc! {"$not": {"$eq": 2}}, sparse).is_none());
    }

    #[test]
    fn a_prefix_regex_bounds_the_strings() {
        let c = Ctx {
            plain_strings: true,
            ..Ctx::default()
        };
        let exact = shown(doc! {"$regex": "^s0"}, c).unwrap();
        assert_eq!(exact.0, ["[\"s0\", \"s1\")", "[/^s0/, /^s0/]"]);
        assert_eq!(exact.1, Tight::Exact);
        let loose = shown(doc! {"$regex": "s0"}, c).unwrap();
        assert_eq!(loose.0, ["[\"\", {})", "[/s0/, /s0/]"]);
        assert_eq!(loose.1, Tight::IndexFilter);
        let escaped = shown(doc! {"$regex": "^s\\.0"}, c).unwrap();
        assert_eq!(escaped.0[0], "[\"s.0\", \"s.1\")");
        // An index of collation keys is not read for a regex at all.
        assert!(shown(doc! {"$regex": "^s0"}, Ctx::default()).is_none());
    }

    #[test]
    fn descending_bounds_print_high_to_low() {
        let (list, _) = leaf_bounds(&doc! {"$in": [1, 3]}, Ctx::default()).unwrap();
        let texts: Vec<Bson> = render_bounds(&list, true);
        assert_eq!(texts, [Bson::from("[3, 3]"), Bson::from("[1, 1]")]);
        let (list, _) = leaf_bounds(&doc! {"$gt": 2}, Ctx::default()).unwrap();
        assert_eq!(render_bounds(&list, true), [Bson::from("[inf, 2)")]);
    }

    #[test]
    fn a_collated_query_cannot_bound_a_string() {
        let c = Ctx {
            collated: true,
            ..Ctx::default()
        };
        assert!(shown(doc! {"$eq": "x"}, c).is_none());
        assert!(shown(doc! {"$in": [1, "x"]}, c).is_none());
        assert!(shown(doc! {"$eq": 2}, c).is_some());
    }
}

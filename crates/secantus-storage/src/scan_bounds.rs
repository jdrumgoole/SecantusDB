//! Which stretches of an index's entries can hold a document a predicate
//! matches -- the bounds the storage router reads an index between.
//!
//! The rules are mongod's planner's (`secantus-commands`' `explain/bounds.rs`
//! computes the same bounds to PRINT them), with one difference that decides
//! everything here: these bounds SELECT documents, so each has to be a
//! superset of the matches as THIS store keys its entries. The caller always
//! runs the whole filter over what the bounds return; a bound that is too
//! wide costs a read, one that is too narrow loses a document. So where a key
//! cannot be trusted to sort where its value does, a stretch is widened to
//! its type bracket rather than cut:
//!
//! - A string, JavaScript or regex key has no terminator, so inverting it for
//!   a DESCENDING index does not reverse a prefix: inverted, `"s"` still
//!   sorts before `"sa"`. A range end of those types on a descending field is
//!   widened to the bracket. A string end holding NUL is widened everywhere,
//!   because NUL bytes run into the compound separator.
//! - An equality's array operand also bounds by its first element, as
//!   mongod's does, for an array the path makes by walking through one.
//!
//! Every value end is INCLUSIVE, even for `$gt`: a key that only ENCODES
//! equal to the bound (a `Decimal128("0.1")` and a double `0.1` do) is then
//! read and rechecked instead of being skipped.
//!
//! A predicate that matches a MISSING field (`$ne`, `$nin`, `$not`,
//! `$exists: false`, an equality to null) needs every document to have an
//! entry: the caller keeps such predicates off sparse indexes, partial
//! indexes it does not imply, and dotted multikey paths
//! (`predicate_may_match_missing`).

use bson::{Bson, Document};
use secantus_core::sortkey::encode_value;

/// One end of a stretch, in ASCENDING key order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// The edge of a type rank: as a low end, every key of that rank and
    /// above; as a high end, every key below that rank.
    Floor(u8),
    /// A value's ascending key: as either end, inclusive.
    Key(Vec<u8>),
}

/// A stretch of keys, low to high; `None` is unbounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stretch {
    pub lo: Option<End>,
    pub hi: Option<End>,
}

impl Stretch {
    fn all() -> Self {
        Stretch { lo: None, hi: None }
    }

    fn point(key: Vec<u8>) -> Self {
        Stretch {
            lo: Some(End::Key(key.clone())),
            hi: Some(End::Key(key)),
        }
    }

    fn bracket(rank: u8) -> Self {
        Stretch {
            lo: Some(End::Floor(rank)),
            hi: Some(End::Floor(rank + 1)),
        }
    }

    /// The single key this stretch is, when it is one.
    pub fn as_point(&self) -> Option<&[u8]> {
        match (&self.lo, &self.hi) {
            (Some(End::Key(a)), Some(End::Key(b))) if a == b => Some(a),
            _ => None,
        }
    }

    /// Where the stretch starts, as bytes every key it holds is at or above.
    fn lo_pos(&self) -> Vec<u8> {
        match &self.lo {
            None => Vec::new(),
            Some(End::Floor(r)) => vec![*r],
            Some(End::Key(k)) => k.clone(),
        }
    }

    /// Where it ends, as bytes every key it holds is below (`None`: never).
    /// A key end takes the key's extensions too (`k` then `0xff`): no key
    /// holds `0xff` after a value's own bytes, see `to_spans`.
    fn hi_pos(&self) -> Option<Vec<u8>> {
        match &self.hi {
            None => None,
            Some(End::Floor(r)) => Some(vec![*r]),
            Some(End::Key(k)) => {
                let mut b = k.clone();
                b.push(0xFF);
                Some(b)
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.hi_pos().is_some_and(|hi| self.lo_pos() >= hi)
    }

    fn intersect(&self, other: &Stretch) -> Option<Stretch> {
        let lo = if self.lo_pos() >= other.lo_pos() {
            self.lo.clone()
        } else {
            other.lo.clone()
        };
        let hi = match (self.hi_pos(), other.hi_pos()) {
            (None, _) => other.hi.clone(),
            (_, None) => self.hi.clone(),
            (Some(a), Some(b)) => {
                if a <= b {
                    self.hi.clone()
                } else {
                    other.hi.clone()
                }
            }
        };
        let out = Stretch { lo, hi };
        (!out.is_empty()).then_some(out)
    }
}

/// What a predicate bounds an index to.
#[derive(Clone, Debug)]
pub(crate) struct Bounds {
    pub stretches: Vec<Stretch>,
    /// A wide read (a negation, a type, the whole index): kept for a field
    /// no narrower predicate of the filter can route.
    pub broad: bool,
}

impl Bounds {
    /// Every stretch is one key that is neither null nor a document or an
    /// array: on a multikey index a document then comes out at the first of
    /// those keys it has, which is where mongod's scan returns it.
    pub fn scalar_points(&self) -> bool {
        let (null, doc, arr) = (rank(&Bson::Null), rank_doc(), rank_array());
        !self.stretches.is_empty()
            && self.stretches.iter().all(|s| {
                s.as_point().is_some_and(
                    |k| !matches!(k.first(), Some(r) if *r == null || *r == doc || *r == arr),
                )
            })
    }

    /// Replace every key end of `ranks` -- points included -- by the edges
    /// of its rank. For the `_id` index, where a document `_id` re-keyed from
    /// `_id` key format 1 may keep its raw-BSON key (`Storage::upgrade_id_keys`).
    pub fn widen_ranks(&mut self, ranks: &[u8]) {
        for s in &mut self.stretches {
            if let Some(End::Key(k)) = &s.lo {
                if ranks.contains(&k[0]) {
                    s.lo = Some(End::Floor(k[0]));
                }
            }
            if let Some(End::Key(k)) = &s.hi {
                if ranks.contains(&k[0]) {
                    s.hi = Some(End::Floor(k[0] + 1));
                }
            }
        }
    }
}

/// The type rank of a value's key.
pub(crate) fn rank(v: &Bson) -> u8 {
    encode_value(v, None).map(|k| k[0]).unwrap_or(0)
}

fn rank_number() -> u8 {
    rank(&Bson::Int32(0))
}

fn rank_string() -> u8 {
    rank(&Bson::String(String::new()))
}

pub(crate) fn rank_doc() -> u8 {
    rank(&Bson::Document(Document::new()))
}

pub(crate) fn rank_array() -> u8 {
    rank(&Bson::Array(Vec::new()))
}

fn rank_regex() -> u8 {
    rank(&Bson::RegularExpression(bson::Regex {
        pattern: String::new(),
        options: String::new(),
    }))
}

fn rank_js() -> u8 {
    rank(&Bson::JavaScriptCode(String::new()))
}

/// The ranks whose keys have no terminator (see the module notes).
pub(crate) fn open_ended_rank(r: u8) -> bool {
    r == rank_string() || r == rank_js() || r == rank_regex()
}

fn key(v: &Bson) -> Option<Vec<u8>> {
    encode_value(v, None).ok()
}

fn is_nan(v: &Bson) -> bool {
    match v {
        Bson::Double(d) => d.is_nan(),
        Bson::Decimal128(d) => d.to_string().to_lowercase().contains("nan"),
        _ => false,
    }
}

/// Whether a key of `v` sorts where `v` does among the other values of its
/// rank, so that it can END a range.
fn trusted_end(v: &Bson, desc: bool) -> bool {
    if key(v).is_none() {
        return false;
    }
    match v {
        Bson::String(s) => !s.contains('\0') && !desc,
        Bson::JavaScriptCode(s) => !s.contains('\0') && !desc,
        Bson::JavaScriptCodeWithScope(c) => !c.code.contains('\0') && !desc,
        Bson::RegularExpression(_) => !desc,
        Bson::Symbol(_) | Bson::Undefined | Bson::DbPointer(_) => false,
        _ => true,
    }
}

/// `[v, rest of its rank]` for `$gt` / `$gte`, `[its rank, v]` for `$lt` /
/// `$lte`. `None` for an operand that bounds nothing here.
fn range(op: &str, v: &Bson, desc: bool) -> Option<Vec<Stretch>> {
    match v {
        Bson::MinKey | Bson::MaxKey => return Some(vec![Stretch::all()]),
        // Only null itself, and a missing field, compare equal to null.
        Bson::Null => return Some(vec![Stretch::point(key(v)?)]),
        Bson::Array(_)
        | Bson::RegularExpression(_)
        | Bson::Undefined
        | Bson::Symbol(_)
        | Bson::DbPointer(_) => return None,
        _ => {}
    }
    let k = key(v)?;
    let r = k[0];
    let lower = op == "$gt" || op == "$gte";
    let end = if trusted_end(v, desc) {
        End::Key(k)
    } else if lower {
        End::Floor(r)
    } else {
        End::Floor(r + 1)
    };
    Some(vec![if lower {
        Stretch {
            lo: Some(end),
            hi: Some(End::Floor(r + 1)),
        }
    } else {
        Stretch {
            lo: Some(End::Floor(r)),
            hi: Some(end),
        }
    }])
}

/// mongod's literal prefix of a regex: `^` and then plain characters, with
/// no flags and no alternation anywhere. `None`: no prefix bounds it.
fn regex_prefix(pattern: &str, options: &str) -> Option<String> {
    if !options.is_empty() || pattern.contains('|') {
        return None;
    }
    let rest = pattern.strip_prefix('^')?;
    let chars: Vec<char> = rest.chars().collect();
    let mut prefix = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let literal = if c == '\\' {
            match chars.get(i + 1) {
                Some(n) if !n.is_alphanumeric() && *n != '\0' => {
                    i += 1;
                    *n
                }
                _ => break,
            }
        } else if ".*+?|()[]{}^$\0".contains(c) {
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
    (!prefix.is_empty()).then_some(prefix)
}

/// The strings a regex can match, and a stored regex equal to it.
fn regex(pattern: &str, options: &str, desc: bool) -> Option<(Vec<Stretch>, bool)> {
    let re = Bson::RegularExpression(bson::Regex {
        pattern: pattern.to_string(),
        options: options.to_string(),
    });
    let strings = rank_string();
    let mut broad = true;
    let mut text = Stretch::bracket(strings);
    if let Some(prefix) = regex_prefix(pattern, options) {
        let mut next: Vec<char> = prefix.chars().collect();
        let last = next.pop()?;
        let bumped = (last as u32 + 1..=char::MAX as u32).find_map(char::from_u32);
        if let Some(b) = bumped {
            next.push(b);
            let lo = Bson::String(prefix);
            let hi = Bson::String(next.into_iter().collect());
            if trusted_end(&lo, desc) && trusted_end(&hi, desc) {
                text = Stretch {
                    lo: Some(End::Key(key(&lo)?)),
                    hi: Some(End::Key(key(&hi)?)),
                };
                broad = false;
            }
        }
    }
    Some((vec![text, Stretch::point(key(&re)?)], broad))
}

/// An equality's keys.
fn equality(v: &Bson, desc: bool) -> Option<(Vec<Stretch>, bool)> {
    match v {
        Bson::RegularExpression(re) => regex(&re.pattern, &re.options, desc),
        Bson::Array(items) => {
            // The whole array, and (as mongod bounds it) its first element:
            // `{"p.q": [1, 2]}` can match an array the path makes from
            // `{p: [{q: 1}, {q: 2}]}`, whose entries are 1 and 2.
            let mut out = vec![Stretch::point(key(v)?)];
            if let Some(first) = items.first() {
                out.push(Stretch::point(key(first)?));
            }
            Some((out, false))
        }
        Bson::Undefined | Bson::Symbol(_) | Bson::DbPointer(_) => None,
        _ => Some((vec![Stretch::point(key(v)?)], false)),
    }
}

/// The complement of one range, for `$not`. The inner range is EXACT here
/// (exclusive where the operator is), so its complement keeps every value
/// the operator rejects; then its value end is made inclusive. `None`: no
/// range this can complement, and the caller reads the whole index.
fn not_range(op: &str, v: &Bson, desc: bool) -> Option<Vec<Stretch>> {
    if is_nan(v) || !trusted_end(v, false) {
        return None;
    }
    if matches!(
        v,
        Bson::Null
            | Bson::MinKey
            | Bson::MaxKey
            | Bson::Array(_)
            | Bson::RegularExpression(_)
            | Bson::JavaScriptCode(_)
            | Bson::JavaScriptCodeWithScope(_)
    ) {
        return None;
    }
    let k = key(v)?;
    let r = k[0];
    let end = if trusted_end(v, desc) {
        End::Key(k)
    } else {
        // On a descending field the value cannot cut its bracket: keep it.
        if op == "$gt" || op == "$gte" {
            End::Floor(r + 1)
        } else {
            End::Floor(r)
        }
    };
    let mut out = if op == "$gt" || op == "$gte" {
        vec![
            Stretch {
                lo: None,
                hi: Some(end),
            },
            Stretch {
                lo: Some(End::Floor(r + 1)),
                hi: None,
            },
        ]
    } else {
        vec![
            Stretch {
                lo: None,
                hi: Some(End::Floor(r)),
            },
            Stretch {
                lo: Some(end),
                hi: None,
            },
        ]
    };
    // A range never matches NaN, so its complement always does.
    if r == rank_number() {
        out.push(Stretch::point(key(&Bson::Double(f64::NAN))?));
    }
    Some(out)
}

/// `$type`'s argument as the ranks it reads, or the whole index.
fn type_ranks(spec: &Bson) -> Option<Vec<Stretch>> {
    let items: Vec<&Bson> = match spec {
        Bson::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in items {
        let code: i64 = match item {
            Bson::String(s) => match s.as_str() {
                "double" | "int" | "long" | "decimal" | "number" => 1,
                "string" => 2,
                "object" => 3,
                "array" => 4,
                "binData" => 5,
                "objectId" => 7,
                "bool" => 8,
                "date" => 9,
                "null" => 10,
                "regex" => 11,
                "javascript" | "javascriptWithScope" => 13,
                "timestamp" => 17,
                "minKey" => -1,
                "maxKey" => 127,
                _ => return None,
            },
            Bson::Int32(n) => *n as i64,
            Bson::Int64(n) => *n,
            Bson::Double(d) if d.fract() == 0.0 => *d as i64,
            _ => return None,
        };
        let one = |v: Bson| Stretch::bracket(rank(&v));
        out.push(match code {
            1 | 16 | 18 | 19 => one(Bson::Int32(0)),
            2 => one(Bson::String(String::new())),
            3 => one(Bson::Document(Document::new())),
            // An array field is read through its elements and its whole-array
            // key alike: every key of the index can be one.
            4 => return Some(vec![Stretch::all()]),
            5 => one(Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: Vec::new(),
            })),
            7 => one(Bson::ObjectId(bson::oid::ObjectId::from_bytes([0; 12]))),
            8 => one(Bson::Boolean(false)),
            9 => one(Bson::DateTime(bson::DateTime::from_millis(0))),
            10 => Stretch::point(key(&Bson::Null)?),
            11 => Stretch::bracket(rank_regex()),
            13 | 15 => Stretch::bracket(rank_js()),
            17 => one(Bson::Timestamp(bson::Timestamp {
                time: 0,
                increment: 0,
            })),
            -1 => Stretch::point(key(&Bson::MinKey)?),
            127 => Stretch::point(key(&Bson::MaxKey)?),
            _ => return None,
        });
    }
    (!out.is_empty()).then_some(out)
}

/// Pairwise overlaps of two lists.
fn intersect(a: &[Stretch], b: &[Stretch]) -> Vec<Stretch> {
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

/// One operator's stretches, and whether they are a wide read.
fn operator(op: &str, arg: &Bson, whole: &Document, desc: bool) -> Option<(Vec<Stretch>, bool)> {
    match op {
        "$eq" => equality(arg, desc),
        "$gt" | "$gte" | "$lt" | "$lte" => range(op, arg, desc).map(|s| (s, false)),
        "$in" => {
            let mut out = Vec::new();
            let mut broad = false;
            for item in arg.as_array()? {
                let (s, b) = equality(item, desc)?;
                out.extend(s);
                broad |= b;
            }
            Some((out, broad))
        }
        // Each matches where the value is absent or unequal, which can be
        // anywhere in the index.
        "$ne" | "$nin" => Some((vec![Stretch::all()], true)),
        "$not" => {
            let inner = match arg {
                Bson::Document(d) if d.len() == 1 => d,
                _ => return Some((vec![Stretch::all()], true)),
            };
            let (iop, iv) = inner.iter().next()?;
            match iop.as_str() {
                "$gt" | "$gte" | "$lt" | "$lte" => Some((
                    not_range(iop, iv, desc).unwrap_or_else(|| vec![Stretch::all()]),
                    true,
                )),
                _ => Some((vec![Stretch::all()], true)),
            }
        }
        "$exists" => {
            let present = match arg {
                Bson::Boolean(b) => *b,
                Bson::Null => false,
                Bson::Int32(n) => *n != 0,
                Bson::Int64(n) => *n != 0,
                Bson::Double(d) => *d != 0.0,
                _ => true,
            };
            if present {
                Some((vec![Stretch::all()], true))
            } else {
                // A missing field is keyed as null.
                Some((vec![Stretch::point(key(&Bson::Null)?)], false))
            }
        }
        "$type" => type_ranks(arg).map(|s| (s, true)),
        "$mod" => Some((vec![Stretch::bracket(rank_number())], true)),
        "$regex" => {
            let (pattern, mut options) = match arg {
                Bson::String(s) => (s.clone(), String::new()),
                Bson::RegularExpression(re) => (re.pattern.clone(), re.options.clone()),
                _ => return None,
            };
            if let Ok(o) = whole.get_str("$options") {
                options = o.to_string();
            }
            regex(&pattern, &options, desc)
        }
        "$all" => {
            // Every element has to be there, so any one bounds it.
            let first = arg.as_array()?.first()?;
            if matches!(first, Bson::Document(d) if d.keys().any(|k| k.starts_with('$'))) {
                return None;
            }
            equality(first, desc)
        }
        "$elemMatch" => {
            // The value form: every operator holds of the SAME element, so
            // their stretches intersect even on a multikey index.
            let inner = arg.as_document()?;
            if inner.is_empty() || inner.keys().any(|k| !k.starts_with('$')) {
                return None;
            }
            // A nested `$elemMatch` tests an element that is itself an
            // array, whose key is that whole array, not its values.
            if inner.contains_key("$elemMatch") {
                return None;
            }
            let (list, broad) = operators(inner, false, desc)?;
            Some((list, broad))
        }
        _ => None,
    }
}

/// The operators of one operator document. Each is a condition every match
/// meets, so one that bounds nothing is left out. On a multikey index two
/// operators can be met by different elements, so only one is used.
fn operators(d: &Document, multikey: bool, desc: bool) -> Option<(Vec<Stretch>, bool)> {
    let mut picked: Option<(Vec<Stretch>, bool)> = None;
    for (op, arg) in d {
        if op == "$options" {
            continue;
        }
        let Some((list, broad)) = operator(op, arg, d, desc) else {
            continue;
        };
        picked = Some(match picked {
            None => (list, broad),
            Some((have, have_broad)) if multikey => {
                // The narrower of the two.
                if have_broad && !broad {
                    (list, broad)
                } else {
                    (have, have_broad)
                }
            }
            Some((have, have_broad)) => (intersect(&have, &list), have_broad && broad),
        });
    }
    picked
}

/// The bounds `pred` -- a field's value in a filter, an equality or an
/// operator document -- puts on an index of that field. `None`: the
/// predicate bounds nothing here and the caller reads another way.
pub(crate) fn for_predicate(pred: &Bson, multikey: bool, desc: bool) -> Option<Bounds> {
    let (stretches, broad) = match pred {
        Bson::Document(d) if !d.is_empty() && d.keys().all(|k| k.starts_with('$')) => {
            operators(d, multikey, desc)?
        }
        // A document with an operator among plain fields is not a query.
        Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => return None,
        other => equality(other, desc)?,
    };
    let stretches: Vec<Stretch> = stretches.into_iter().filter(|s| !s.is_empty()).collect();
    Some(Bounds { stretches, broad })
}

/// One stretch of an index's entries as stored: from `lo` (inclusive) to
/// `hi` (exclusive), on the escaped (or, for the `_id` index, raw) bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub lo: Option<Vec<u8>>,
    pub hi: Option<Vec<u8>>,
}

/// The stored spans of `bounds` on a field of direction `dir`, low to high
/// in the order the entries are stored, overlaps merged. `escape` is the
/// entry escaping (`None` for raw keys).
///
/// A key end's high side is `key + 0xff`: that takes the key itself in a
/// compound index (where `0x00 0x00` and the next field follow it) and the
/// keys that extend it, none of which holds `0xff` next (no UTF-8 text does,
/// and every other key is terminated).
pub(crate) fn to_spans(bounds: &Bounds, dir: i32, escape: fn(&[u8]) -> Vec<u8>) -> Vec<Span> {
    let floor = |r: u8| -> Vec<u8> {
        if dir == -1 {
            vec![0xFFu8 - r + 1]
        } else {
            vec![r]
        }
    };
    // A descending field stores the inverted key and its terminator
    // (`sortkey::encode_value_directed`, entry format 6).
    let key_bytes = |k: &[u8]| -> Vec<u8> {
        if dir == -1 {
            let mut inverted: Vec<u8> = k.iter().map(|b| b ^ 0xFF).collect();
            inverted.extend_from_slice(secantus_core::sortkey::DESC_END);
            escape(&inverted)
        } else {
            escape(k)
        }
    };
    let low = |e: &End| match e {
        End::Floor(r) => floor(*r),
        End::Key(k) => key_bytes(k),
    };
    let high = |e: &End| match e {
        End::Floor(r) => floor(*r),
        End::Key(k) => {
            let mut b = key_bytes(k);
            b.push(0xFF);
            b
        }
    };
    let mut spans: Vec<Span> = bounds
        .stretches
        .iter()
        .map(|s| {
            if dir == -1 {
                Span {
                    lo: s.hi.as_ref().map(low),
                    hi: s.lo.as_ref().map(high),
                }
            } else {
                Span {
                    lo: s.lo.as_ref().map(low),
                    hi: s.hi.as_ref().map(high),
                }
            }
        })
        .filter(|s| match (&s.lo, &s.hi) {
            (Some(l), Some(h)) => l < h,
            _ => true,
        })
        .collect();
    spans.sort_by(|a, b| a.lo.cmp(&b.lo));
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        if let Some(last) = out.last_mut() {
            let touches = match (&last.hi, &s.lo) {
                (None, _) => true,
                (Some(_), None) => true,
                (Some(h), Some(l)) => l <= h,
            };
            if touches {
                last.hi = match (&last.hi, &s.hi) {
                    (None, _) | (_, None) => None,
                    (Some(a), Some(b)) => Some(a.max(b).clone()),
                };
                continue;
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn bounds(pred: Bson) -> Bounds {
        for_predicate(&pred, false, false).expect("bounds")
    }

    fn plain(k: &[u8]) -> Vec<u8> {
        k.to_vec()
    }

    #[test]
    fn negations_read_everything() {
        for pred in [
            Bson::Document(doc! {"$ne": 5}),
            Bson::Document(doc! {"$nin": [1, 2]}),
            Bson::Document(doc! {"$not": {"$eq": 5}}),
            Bson::Document(doc! {"$exists": true}),
            Bson::Document(doc! {"$type": "array"}),
        ] {
            let b = bounds(pred);
            assert!(b.broad);
            assert_eq!(to_spans(&b, 1, plain), vec![Span { lo: None, hi: None }]);
        }
    }

    #[test]
    fn a_complement_keeps_the_bound_and_nan() {
        let b = bounds(Bson::Document(doc! {"$not": {"$gt": 2}}));
        let two = encode_value(&Bson::Int32(2), None).unwrap();
        let nan = encode_value(&Bson::Double(f64::NAN), None).unwrap();
        // [open, 2] (2 itself is not > 2), then from the strings on.
        assert!(b.stretches.contains(&Stretch {
            lo: None,
            hi: Some(End::Key(two))
        }));
        assert!(b.stretches.contains(&Stretch::point(nan)));
    }

    #[test]
    fn a_range_end_is_inclusive_at_any_exponent() {
        let b = bounds(Bson::Document(doc! {"$gt": 2}));
        let two = encode_value(&Bson::Int32(2), None).unwrap();
        assert_eq!(b.stretches[0].lo, Some(End::Key(two)));
        assert_eq!(b.stretches.len(), 1);
        // Number-key format 2 keys every exponent in order.
        let big = encode_value(&Bson::Double(1e200), None).unwrap();
        let b = bounds(Bson::Document(doc! {"$gt": 1e200}));
        assert_eq!(b.stretches[0].lo, Some(End::Key(big)));
    }

    #[test]
    fn a_descending_string_range_is_its_bracket() {
        let b = for_predicate(&Bson::Document(doc! {"$gt": "s"}), false, true).unwrap();
        assert_eq!(b.stretches[0].lo, Some(End::Floor(rank_string())));
        // A point is still a point: an equal key is inside it either way.
        let b = for_predicate(&Bson::String("s".into()), false, true).unwrap();
        assert!(b.stretches[0].as_point().is_some());
    }

    #[test]
    fn a_prefix_regex_needs_no_alternation() {
        let b = bounds(Bson::Document(doc! {"$regex": "^s0"}));
        assert!(!b.broad);
        let b = bounds(Bson::Document(doc! {"$regex": "^s|t"}));
        assert!(b.broad);
        assert_eq!(b.stretches[0], Stretch::bracket(rank_string()));
        let b = bounds(Bson::Document(doc! {"$regex": "^s", "$options": "i"}));
        assert!(b.broad);
    }

    #[test]
    fn elem_match_intersects_and_all_uses_one_element() {
        let b = for_predicate(
            &Bson::Document(doc! {"$elemMatch": {"$gt": 4, "$lt": 6}}),
            true,
            false,
        )
        .unwrap();
        let four = encode_value(&Bson::Int32(4), None).unwrap();
        let six = encode_value(&Bson::Int32(6), None).unwrap();
        assert_eq!(
            b.stretches[0],
            Stretch {
                lo: Some(End::Key(four)),
                hi: Some(End::Key(six))
            }
        );
        let b = for_predicate(&Bson::Document(doc! {"$all": [5, 6]}), true, false).unwrap();
        assert!(b.scalar_points());
    }

    #[test]
    fn descending_spans_run_high_to_low() {
        let b = bounds(Bson::Document(doc! {"$in": [1, 3]}));
        let spans = to_spans(&b, -1, plain);
        assert_eq!(spans.len(), 2);
        // The stored key of 3 (inverted, terminated) comes first.
        let three = secantus_core::sortkey::encode_value_directed(&Bson::Int32(3), -1, None)
            .unwrap();
        assert_eq!(spans[0].lo, Some(three));
    }
}

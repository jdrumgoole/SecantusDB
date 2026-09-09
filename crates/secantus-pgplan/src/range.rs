//! Range types, and the canonicalisation that makes two of them equal.
//!
//! A range over a DISCRETE element type has one true spelling: PostgreSQL
//! rewrites every bound to `[)`, so `'[1,5]'::int4range` is stored and printed
//! as `[1,6)` and `'(1,5)'` as `[2,5)`. Over a CONTINUOUS type there is no such
//! rewrite — `[1.0,2.0]` stays inclusive, because there is no "next" number to
//! move the bound to.
//!
//! That split is the whole design. `int4range`, `int8range` and `daterange` are
//! discrete; `numrange`, `tsrange` and `tstzrange` are not. Getting it wrong
//! makes `int4range(1,5)` and `'[1,5]'::int4range` compare unequal when
//! PostgreSQL says they are the same range.
//!
//! Measured against PostgreSQL 14.

use crate::{cast_value, Error, Result};
use bson::Bson;

/// The element type of each range type, and whether it is discrete.
///
/// A builtin range comes from the table; a CUSTOM one (`create type testrange
/// as range (subtype = text)`) from the user-range registry the wire layer
/// installs per statement. A custom range has no canonical function, so it is
/// never discrete: `[a,c]` over `text` stays `[a,c]`. Resolving both here is
/// what lets every constructor, cast and multirange path treat a custom range
/// exactly as a builtin one.
pub fn range_element(name: &str) -> Option<(String, bool)> {
    let (element, discrete) = match name {
        "int4range" => ("int4", true),
        "int8range" => ("int8", true),
        "daterange" => ("date", true),
        "numrange" => ("numeric", false),
        "tsrange" => ("timestamp", false),
        "tstzrange" => ("timestamptz", false),
        other => return crate::user_range_subtype(other).map(|sub| (sub, false)),
    };
    Some((element.to_string(), discrete))
}

pub fn is_range_type(name: &str) -> bool {
    range_element(name).is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub empty: bool,
    /// `None` is an INFINITE bound, which prints as nothing at all: `(,5)`.
    pub lower: Option<String>,
    pub upper: Option<String>,
    pub lower_inc: bool,
    pub upper_inc: bool,
}

impl Range {
    pub fn empty() -> Range {
        Range {
            empty: true,
            lower: None,
            upper: None,
            lower_inc: false,
            upper_inc: false,
        }
    }
}

/// Render a range as PostgreSQL prints it. An infinite bound is empty text, so
/// an unbounded lower end is `(,5)` rather than `(-infinity,5)`.
pub fn render(r: &Range) -> String {
    if r.empty {
        return "empty".to_string();
    }
    format!(
        "{}{},{}{}",
        if r.lower_inc { '[' } else { '(' },
        quote_bound(r.lower.as_deref()),
        quote_bound(r.upper.as_deref()),
        if r.upper_inc { ']' } else { ')' }
    )
}

/// Quote a bound whose text would otherwise be ambiguous inside the brackets:
/// anything containing a comma, a quote, a backslash, whitespace or a bracket.
/// A timestamp bound always needs it, since it has a space in the middle.
fn quote_bound(b: Option<&str>) -> String {
    let Some(text) = b else {
        return String::new();
    };
    let needs = text.is_empty()
        || text.chars().any(|c| {
            matches!(c, ',' | '"' | '\\' | '(' | ')' | '[' | ']') || c.is_ascii_whitespace()
        });
    if !needs {
        return text.to_string();
    }
    // Inside the quotes PostgreSQL DOUBLES a quote and backslash-escapes a
    // backslash (`range_out`): the bound `"` prints as `""""`, `\` as `"\\"`.
    let mut out = String::from('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\"\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse a range literal: `[1,5)`, `(,5]`, `empty`, with optional quoting
/// around a bound that contains a comma or a quote.
fn parse_literal(text: &str) -> Result<Range> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("empty") {
        return Ok(Range::empty());
    }
    let bytes = t.as_bytes();
    let (lower_inc, upper_inc) = match (bytes.first(), bytes.last()) {
        (Some(b'['), Some(b')')) => (true, false),
        (Some(b'['), Some(b']')) => (true, true),
        (Some(b'('), Some(b')')) => (false, false),
        (Some(b'('), Some(b']')) => (false, true),
        _ => return Err(bad_range(text)),
    };
    let body = &t[1..t.len() - 1];
    // Each part carries whether it was ever quoted: `["",foo)` has an EMPTY
    // STRING lower bound, where `[,foo)` has an infinite one, and only the
    // quotes tell them apart once the text is stripped.
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut seen_quote = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A doubled quote inside the quotes is one literal quote, the
            // way PostgreSQL's `range_parse_bound` reads it: `["""",#)` is
            // the bound `"`.
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                cur.push('"');
            }
            '"' => {
                quoted = !quoted;
                seen_quote = true;
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ',' if !quoted => {
                parts.push((std::mem::take(&mut cur), std::mem::take(&mut seen_quote)));
            }
            // Whitespace outside the quotes is not part of the bound. ASCII only,
            // as C `isspace` reads it: U+0085 and U+00A0 are bound text.
            c if !quoted && c.is_ascii_whitespace() => {}
            _ => cur.push(c),
        }
    }
    parts.push((cur, seen_quote));
    if parts.len() != 2 {
        return Err(bad_range(text));
    }
    let bound = |(s, was_quoted): &(String, bool)| {
        if s.is_empty() && !was_quoted {
            None
        } else {
            Some(s.clone())
        }
    };
    Ok(Range {
        empty: false,
        lower: bound(&parts[0]),
        upper: bound(&parts[1]),
        lower_inc,
        upper_inc,
    })
}

fn bad_range(text: &str) -> Error {
    Error::InvalidText(format!("malformed range literal: \"{}\"", text.trim()))
}

/// Build the canonical range of `type_name` from a literal.
pub fn from_text(text: &str, type_name: &str) -> Result<Range> {
    let (element, discrete) = range_element(type_name).ok_or_else(|| bad_range(text))?;
    let parsed = parse_literal(text)?;
    canonicalise(parsed, &element, discrete, type_name)
}

/// Parse a range literal for a KNOWN element type (a custom range's subtype),
/// canonicalising only when `discrete` (a custom range has no canonical
/// function, so callers pass `false`).
pub fn from_text_element(text: &str, element: &str, discrete: bool) -> Result<Range> {
    let parsed = parse_literal(text)?;
    canonicalise(parsed, element, discrete, element)
}

/// Build from constructor arguments: `int4range(lo, hi)` and its three-argument
/// form, where the third names the bounds as one of `[]`, `[)`, `(]`, `()`.
pub fn from_args(args: &[Bson], type_name: &str, null_flags_is_error: bool) -> Result<Range> {
    let (element, discrete) = range_element(type_name)
        .ok_or_else(|| Error::Unsupported(format!("the {type_name} type")))?;
    if args.is_empty() || args.len() > 3 {
        return Err(Error::Parse(format!(
            "function {type_name} does not exist with that argument list"
        )));
    }
    let bounds = match args.get(2) {
        Some(Bson::String(s)) => s.clone(),
        None => "[)".to_string(),
        // A NULL flags argument is an error in PostgreSQL -- but at DESCRIBE
        // time every parameter IS null, because Describe runs before Bind. So
        // the caller says whether this NULL came from a placeholder (plan it
        // as an unknown, and let Bind supply the real value) or from a literal
        // `null` in the query (a real error). Without that split, every
        // `int4range(%s, %s, %s)` failed at describe with a message about a
        // malformed literal that named this server's own placeholder text.
        Some(Bson::Null) if !null_flags_is_error => return Ok(Range::empty()),
        Some(Bson::Null) => {
            return Err(Error::DataException(
                "range constructor flags argument must not be null".to_string(),
            ))
        }
        Some(_) => {
            return Err(Error::Parse(
                "range constructor flags argument must be text".to_string(),
            ))
        }
    };
    let (lower_inc, upper_inc) = match bounds.as_str() {
        "[]" => (true, true),
        "[)" => (true, false),
        "(]" => (false, true),
        "()" => (false, false),
        // PostgreSQL puts bad bound flags in the SYNTAX class, not the
        // invalid-text one where a malformed literal goes.
        other => {
            return Err(Error::Parse(format!(
                "invalid range bound flags: \"{other}\""
            )))
        }
    };
    let text_of = |v: Option<&Bson>| match v {
        None | Some(Bson::Null) => None,
        Some(other) => Some(crate::render_value_text(other)),
    };
    let r = Range {
        empty: false,
        lower: text_of(args.first()),
        upper: text_of(args.get(1)),
        lower_inc,
        upper_inc,
    };
    canonicalise(r, &element, discrete, type_name)
}

/// Normalise a range: reject a crossed pair, collapse an empty one, and — for a
/// DISCRETE element type only — rewrite the bounds to `[)`.
fn canonicalise(mut r: Range, element: &str, discrete: bool, type_name: &str) -> Result<Range> {
    if r.empty {
        return Ok(r);
    }
    // Each bound is stored as the element type's own canonical text, so two
    // spellings of one value (`1.0` and `1.00`) do not make two ranges.
    let cast_bound = |b: &Option<String>| -> Result<Option<String>> {
        match b {
            None => Ok(None),
            Some(t) => Ok(Some(crate::render_value_text(&cast_value(
                Bson::String(t.clone()),
                element,
            )?))),
        }
    };
    r.lower = cast_bound(&r.lower)?;
    r.upper = cast_bound(&r.upper)?;

    if discrete {
        // `(x` becomes `[x+1`, and `y]` becomes `y+1)`.
        if !r.lower_inc {
            if let Some(l) = &r.lower {
                r.lower = Some(step(l, element, 1)?);
            }
            r.lower_inc = true;
        }
        if r.upper_inc {
            if let Some(u) = &r.upper {
                r.upper = Some(step(u, element, 1)?);
            }
            r.upper_inc = false;
        }
    }
    // An infinite bound is always exclusive, for EVERY range type: PostgreSQL's
    // `range_serialize` clears the flag, so `'[,foo)'::textrange` prints as
    // `(,foo)`. Inside the discrete block this left a text-subtype range
    // carrying `[,foo)`, a form PostgreSQL never prints.
    if r.lower.is_none() {
        r.lower_inc = false;
    }
    if r.upper.is_none() {
        r.upper_inc = false;
    }

    if let (Some(l), Some(u)) = (&r.lower, &r.upper) {
        let ord = compare_bounds(l, u, element)?;
        if ord == std::cmp::Ordering::Greater {
            return Err(Error::DataException(
                "range lower bound must be less than or equal to range upper bound".to_string(),
            ));
        }
        // Equal bounds that exclude each other contain nothing.
        if ord == std::cmp::Ordering::Equal && !(r.lower_inc && r.upper_inc) {
            return Ok(Range::empty());
        }
    }
    let _ = type_name;
    Ok(r)
}

fn compare_bounds(a: &str, b: &str, element: &str) -> Result<std::cmp::Ordering> {
    let av = cast_value(Bson::String(a.to_string()), element)?;
    let bv = cast_value(Bson::String(b.to_string()), element)?;
    crate::compare_constants(&av, &bv)
        .ok_or_else(|| Error::Unsupported(format!("comparing {element} range bounds")))
}

/// The next value after `text` in a discrete element type.
fn step(text: &str, element: &str, by: i64) -> Result<String> {
    match element {
        "int4" | "int8" => {
            let n: i64 = text.trim().parse().map_err(|_| bad_range(text))?;
            Ok((n + by).to_string())
        }
        // A date steps by whole days.
        "date" => {
            let micros = crate::parse_timestamp(text)?;
            Ok(crate::render_timestamp(micros + by * 86_400_000_000)
                .split(' ')
                .next()
                .unwrap_or_default()
                .to_string())
        }
        other => Err(Error::Unsupported(format!(
            "stepping a {other} range bound"
        ))),
    }
}

/// The range type behind an oid, for decoding a bound parameter.
pub fn range_oid_name(oid: u32) -> Option<&'static str> {
    Some(match oid {
        3904 => "int4range",
        3926 => "int8range",
        3906 => "numrange",
        3912 => "daterange",
        3908 => "tsrange",
        3910 => "tstzrange",
        _ => return None,
    })
}

/// The oid of a range type's ELEMENT, for decoding its bounds.
pub fn range_element_oid(type_name: &str) -> u32 {
    match type_name {
        "int4range" => 23,
        "int8range" => 20,
        "numrange" => 1700,
        "daterange" => 1082,
        "tsrange" => 1114,
        "tstzrange" => 1184,
        // A custom range's element is whatever its subtype is; the fallback
        // is `text`, which is what a subtype this server cannot name decodes
        // as least wrongly.
        other => range_element(other)
            .and_then(|(e, _)| crate::pgtypes::oid_of_name(&e))
            .and_then(|oid| u32::try_from(oid).ok())
            .unwrap_or(25),
    }
}

// ---------------------------------------------------------------------------
// Bound accessors
// ---------------------------------------------------------------------------

/// The range bound functions: `lower` / `upper` answer the bound in the
/// ELEMENT type (NULL for an infinite bound or an empty range), `lower_inc` /
/// `upper_inc` / `lower_inf` / `upper_inf` / `isempty` answer booleans.
/// Measured against PostgreSQL 16.
pub const ACCESSORS: &[&str] = &[
    "lower",
    "upper",
    "lower_inc",
    "upper_inc",
    "lower_inf",
    "upper_inf",
    "isempty",
];

pub fn is_accessor(name: &str) -> bool {
    ACCESSORS.contains(&name)
}

/// The result type of an accessor over a range whose element is `element`.
pub fn accessor_result_type(name: &str, element: &str) -> String {
    match name {
        "lower" | "upper" => element.to_string(),
        _ => "bool".to_string(),
    }
}

/// Apply an accessor to one range.
pub fn accessor(name: &str, r: &Range, element: &str) -> Result<Bson> {
    let bound = |b: &Option<String>| -> Result<Bson> {
        match b {
            Some(text) if !r.empty => cast_value(Bson::String(text.clone()), element),
            _ => Ok(Bson::Null),
        }
    };
    Ok(match name {
        "lower" => bound(&r.lower)?,
        "upper" => bound(&r.upper)?,
        "lower_inc" => Bson::Boolean(!r.empty && r.lower_inc),
        "upper_inc" => Bson::Boolean(!r.empty && r.upper_inc),
        "lower_inf" => Bson::Boolean(!r.empty && r.lower.is_none()),
        "upper_inf" => Bson::Boolean(!r.empty && r.upper.is_none()),
        "isempty" => Bson::Boolean(r.empty),
        other => return Err(Error::Unsupported(format!("function {other}()"))),
    })
}

/// Apply an accessor to a multirange: the lower end is the first member's,
/// the upper end the last member's, and an empty multirange has neither.
pub fn multirange_accessor(name: &str, members: &[Range], element: &str) -> Result<Bson> {
    let (Some(first), Some(last)) = (members.first(), members.last()) else {
        return accessor(name, &Range::empty(), element);
    };
    match name {
        "upper" | "upper_inc" | "upper_inf" => accessor(name, last, element),
        _ => accessor(name, first, element),
    }
}

// ---------------------------------------------------------------------------
// Multiranges
// ---------------------------------------------------------------------------

/// The name PostgreSQL auto-creates for the multirange companion of a range
/// type. PostgreSQL's rule (`makeMultirangeTypeName`): replace the FIRST
/// occurrence of the substring `range` with `multirange`; if the range name
/// contains no `range`, append `_multirange`. So `testrange` -> `testmultirange`,
/// `int4range` -> `int4multirange`, `rangetest` -> `multirangetest`, and
/// `foo` -> `foo_multirange`. Measured against PostgreSQL 14.
pub fn multirange_name_for(range: &str) -> String {
    match range.find("range") {
        Some(idx) => {
            let mut s = String::with_capacity(range.len() + 5);
            s.push_str(&range[..idx]);
            s.push_str("multirange");
            s.push_str(&range[idx + "range".len()..]);
            s
        }
        None => format!("{range}_multirange"),
    }
}

/// The range type a multirange is built from. A builtin multirange comes
/// from the table; a custom range's auto-created companion (`testmultirange`
/// for `testrange`) from the registry the wire layer installs.
pub fn multirange_member(name: &str) -> Option<String> {
    let member = match name {
        "int4multirange" => "int4range",
        "int8multirange" => "int8range",
        "nummultirange" => "numrange",
        "datemultirange" => "daterange",
        "tsmultirange" => "tsrange",
        "tstzmultirange" => "tstzrange",
        other => return crate::user_multirange_member(other),
    };
    Some(member.to_string())
}

pub fn is_multirange_type(name: &str) -> bool {
    multirange_member(name).is_some()
}

pub fn multirange_oid_name(oid: u32) -> Option<&'static str> {
    Some(match oid {
        4451 => "int4multirange",
        4536 => "int8multirange",
        4532 => "nummultirange",
        4535 => "datemultirange",
        4533 => "tsmultirange",
        4534 => "tstzmultirange",
        _ => return None,
    })
}

/// Split `{r1,r2}` into its members. A range contains commas of its own, so the
/// split has to track brackets and quoting rather than cutting on every comma.
fn split_members(text: &str) -> Result<Vec<String>> {
    let t = text.trim();
    if !t.starts_with('{') || !t.ends_with('}') {
        return Err(bad_multirange(text));
    }
    let body = &t[1..t.len() - 1];
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '\\' if quoted => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '[' | '(' if !quoted => {
                depth += 1;
                cur.push(c);
            }
            ']' | ')' if !quoted => {
                depth -= 1;
                cur.push(c);
            }
            ',' if !quoted && depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if depth != 0 || quoted {
        return Err(bad_multirange(text));
    }
    out.push(cur);
    Ok(out)
}

fn bad_multirange(text: &str) -> Error {
    Error::InvalidText(format!("malformed multirange literal: \"{}\"", text.trim()))
}

/// Normalise a set of ranges the way PostgreSQL stores a multirange: empty
/// members dropped, the rest sorted by lower bound, and any two that OVERLAP
/// **or merely touch** merged into one.
///
/// Adjacency is the part that is easy to miss: `{[1,5),[5,8)}` is `{[1,8)}`,
/// because nothing lies between them — while `{[1,5),[6,8)}` stays two members,
/// because 5 does. So the merge test is "does the next one start at or before
/// this one ends", not "do they overlap".
pub fn normalise_multirange(mut members: Vec<Range>, member_type: &str) -> Result<Vec<Range>> {
    let (element, _) = range_element(member_type)
        .ok_or_else(|| Error::Unsupported(format!("the {member_type} type")))?;
    let element = element.as_str();
    members.retain(|r| !r.empty);
    // An absent lower bound is smaller than any value, so it sorts first.
    let mut sorted: Vec<Range> = Vec::new();
    for m in members {
        let pos = sorted.partition_point(|s| lower_before(s, &m, element).unwrap_or(true));
        sorted.insert(pos, m);
    }
    let mut out: Vec<Range> = Vec::new();
    for m in sorted {
        match out.last_mut() {
            Some(prev) if touches(prev, &m, element)? => {
                // Keep the further of the two upper ends.
                if upper_before(prev, &m, element)? {
                    prev.upper = m.upper.clone();
                    prev.upper_inc = m.upper_inc;
                }
            }
            _ => out.push(m),
        }
    }
    Ok(out)
}

fn lower_before(a: &Range, b: &Range, element: &str) -> Result<bool> {
    Ok(match (&a.lower, &b.lower) {
        (None, None) => a.lower_inc && !b.lower_inc,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(x), Some(y)) => match compare_bounds(x, y, element)? {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            // At the same value, the inclusive bound starts first.
            std::cmp::Ordering::Equal => a.lower_inc && !b.lower_inc,
        },
    })
}

fn upper_before(a: &Range, b: &Range, element: &str) -> Result<bool> {
    Ok(match (&a.upper, &b.upper) {
        (None, None) => false,
        // An absent upper bound reaches further than any value.
        (None, Some(_)) => false,
        (Some(_), None) => true,
        (Some(x), Some(y)) => match compare_bounds(x, y, element)? {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => !a.upper_inc && b.upper_inc,
        },
    })
}

/// Do these two ranges overlap or touch? `a` is known to start no later
/// than `b`.
fn touches(a: &Range, b: &Range, element: &str) -> Result<bool> {
    // `a` runs to infinity, so it reaches everything after it.
    let Some(a_upper) = &a.upper else {
        return Ok(true);
    };
    // `b` starts at negative infinity, so it reaches back into `a`.
    let Some(b_lower) = &b.lower else {
        return Ok(true);
    };
    Ok(match compare_bounds(a_upper, b_lower, element)? {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // They meet at one value. They touch unless BOTH exclude it, which is
        // what leaves a hole between them.
        std::cmp::Ordering::Equal => a.upper_inc || b.lower_inc,
    })
}

pub fn render_multirange(members: &[Range]) -> String {
    format!(
        "{{{}}}",
        members.iter().map(render).collect::<Vec<_>>().join(",")
    )
}

/// A multirange from its literal text.
pub fn multirange_from_text(text: &str, type_name: &str) -> Result<Vec<Range>> {
    let member_type = multirange_member(type_name).ok_or_else(|| bad_multirange(text))?;
    let parts = split_members(text)?;
    let members = parts
        .iter()
        .map(|p| from_text(p, &member_type))
        .collect::<Result<Vec<_>>>()?;
    normalise_multirange(members, &member_type)
}

/// A multirange from constructor arguments, each of which is already a range.
pub fn multirange_from_args(args: &[Bson], type_name: &str) -> Result<Vec<Range>> {
    let member_type = multirange_member(type_name)
        .ok_or_else(|| Error::Unsupported(format!("the {type_name} type")))?;
    let members = args
        .iter()
        .filter(|a| *a != &Bson::Null)
        .map(|a| from_text(&crate::render_value_text(a), &member_type))
        .collect::<Result<Vec<_>>>()?;
    normalise_multirange(members, &member_type)
}

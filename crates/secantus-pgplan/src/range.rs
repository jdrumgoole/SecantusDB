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
pub fn range_element(name: &str) -> Option<(&'static str, bool)> {
    Some(match name {
        "int4range" => ("int4", true),
        "int8range" => ("int8", true),
        "daterange" => ("date", true),
        "numrange" => ("numeric", false),
        "tsrange" => ("timestamp", false),
        "tstzrange" => ("timestamptz", false),
        _ => return None,
    })
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
        || text
            .chars()
            .any(|c| matches!(c, ',' | '"' | '\\' | '(' | ')' | '[' | ']') || c.is_whitespace());
    if !needs {
        return text.to_string();
    }
    let mut out = String::from('"');
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
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
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ',' if !quoted => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    if parts.len() != 2 {
        return Err(bad_range(text));
    }
    let bound = |s: &str| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
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
    canonicalise(parsed, element, discrete, type_name)
}

/// Build from constructor arguments: `int4range(lo, hi)` and its three-argument
/// form, where the third names the bounds as one of `[]`, `[)`, `(]`, `()`.
pub fn from_args(args: &[Bson], type_name: &str) -> Result<Range> {
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
        Some(_) => return Err(bad_range("bounds must be text")),
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
    canonicalise(r, element, discrete, type_name)
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
        // An unbounded lower end is always exclusive in the printed form.
        if r.lower.is_none() {
            r.lower_inc = false;
        }
        if r.upper.is_none() {
            r.upper_inc = false;
        }
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

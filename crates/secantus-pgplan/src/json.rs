//! `json` and `jsonb`, and the one difference that matters between them.
//!
//! `json` stores the text a client sent, VERBATIM: whitespace, key order and
//! duplicate keys all survive, and the type only validates. `jsonb` stores a
//! parsed structure, so it comes back NORMALISED — object keys sorted, the last
//! of any duplicate pair kept, and one canonical spacing.
//!
//! Numbers are the trap. A `jsonb` number is a `numeric`, and comes back in
//! `numeric`'s own spelling: `1.10` stays `1.10` because scale is part of the
//! value, while `-1.5e10` becomes `-15000000000` because `numeric` does not
//! print an exponent. So a parser that turns numbers into `f64` — what a
//! general-purpose JSON library does by default — cannot round-trip `jsonb`:
//! it loses the trailing zero. Number tokens are kept as text and normalised
//! the way `numeric` renders them.
//!
//! All of the above was measured against PostgreSQL 14.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number's ORIGINAL text. See the module note.
    Number(String),
    Str(String),
    Array(Vec<Json>),
    /// Kept as a list rather than a map so the parser can preserve input order
    /// for duplicate handling; normalisation sorts and dedupes.
    Object(Vec<(String, Json)>),
}

#[derive(Debug)]
pub struct ParseError;

/// Parse a complete JSON document. Trailing non-whitespace is an error, so
/// `{"a":1} x` is rejected the way PostgreSQL rejects it.
pub fn parse(text: &str) -> Result<Json, ParseError> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let value = parse_value(bytes, &mut pos)?;
    skip_ws(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(ParseError);
    }
    Ok(value)
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_value(b: &[u8], pos: &mut usize) -> Result<Json, ParseError> {
    skip_ws(b, pos);
    let Some(&c) = b.get(*pos) else {
        return Err(ParseError);
    };
    match c {
        b'{' => parse_object(b, pos),
        b'[' => parse_array(b, pos),
        b'"' => parse_string(b, pos).map(Json::Str),
        b't' => literal(b, pos, "true").map(|()| Json::Bool(true)),
        b'f' => literal(b, pos, "false").map(|()| Json::Bool(false)),
        b'n' => literal(b, pos, "null").map(|()| Json::Null),
        _ => parse_number(b, pos),
    }
}

fn literal(b: &[u8], pos: &mut usize, word: &str) -> Result<(), ParseError> {
    if b.len() < *pos + word.len() || &b[*pos..*pos + word.len()] != word.as_bytes() {
        return Err(ParseError);
    }
    *pos += word.len();
    Ok(())
}

fn parse_number(b: &[u8], pos: &mut usize) -> Result<Json, ParseError> {
    let start = *pos;
    if b.get(*pos) == Some(&b'-') {
        *pos += 1;
    }
    let int_start = *pos;
    while matches!(b.get(*pos), Some(c) if c.is_ascii_digit()) {
        *pos += 1;
    }
    if *pos == int_start {
        return Err(ParseError);
    }
    // A leading zero may not be followed by another digit: `01` is not JSON.
    if b[int_start] == b'0' && *pos - int_start > 1 {
        return Err(ParseError);
    }
    if b.get(*pos) == Some(&b'.') {
        *pos += 1;
        let frac_start = *pos;
        while matches!(b.get(*pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
        if *pos == frac_start {
            return Err(ParseError);
        }
    }
    if matches!(b.get(*pos), Some(b'e' | b'E')) {
        *pos += 1;
        if matches!(b.get(*pos), Some(b'+' | b'-')) {
            *pos += 1;
        }
        let exp_start = *pos;
        while matches!(b.get(*pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
        if *pos == exp_start {
            return Err(ParseError);
        }
    }
    let text = std::str::from_utf8(&b[start..*pos]).map_err(|_| ParseError)?;
    Ok(Json::Number(text.to_string()))
}

fn parse_string(b: &[u8], pos: &mut usize) -> Result<String, ParseError> {
    if b.get(*pos) != Some(&b'"') {
        return Err(ParseError);
    }
    *pos += 1;
    let mut out = String::new();
    loop {
        let Some(&c) = b.get(*pos) else {
            return Err(ParseError);
        };
        match c {
            b'"' => {
                *pos += 1;
                return Ok(out);
            }
            b'\\' => {
                *pos += 1;
                let Some(&e) = b.get(*pos) else {
                    return Err(ParseError);
                };
                *pos += 1;
                match e {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = b.get(*pos..*pos + 4).ok_or(ParseError)?;
                        let hex = std::str::from_utf8(hex).map_err(|_| ParseError)?;
                        let n = u32::from_str_radix(hex, 16).map_err(|_| ParseError)?;
                        *pos += 4;
                        // A surrogate pair is two escapes; anything unpaired is
                        // left as the replacement character rather than failing,
                        // which is what a lone surrogate can only become in
                        // UTF-8 text.
                        out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
                    }
                    _ => return Err(ParseError),
                }
            }
            // A raw control character is not valid inside a JSON string.
            0x00..=0x1f => return Err(ParseError),
            _ => {
                let rest = std::str::from_utf8(&b[*pos..]).map_err(|_| ParseError)?;
                let ch = rest.chars().next().ok_or(ParseError)?;
                out.push(ch);
                *pos += ch.len_utf8();
            }
        }
    }
}

fn parse_array(b: &[u8], pos: &mut usize) -> Result<Json, ParseError> {
    *pos += 1; // '['
    let mut items = Vec::new();
    skip_ws(b, pos);
    if b.get(*pos) == Some(&b']') {
        *pos += 1;
        return Ok(Json::Array(items));
    }
    loop {
        items.push(parse_value(b, pos)?);
        skip_ws(b, pos);
        match b.get(*pos) {
            Some(b',') => *pos += 1,
            Some(b']') => {
                *pos += 1;
                return Ok(Json::Array(items));
            }
            _ => return Err(ParseError),
        }
    }
}

fn parse_object(b: &[u8], pos: &mut usize) -> Result<Json, ParseError> {
    *pos += 1; // '{'
    let mut pairs = Vec::new();
    skip_ws(b, pos);
    if b.get(*pos) == Some(&b'}') {
        *pos += 1;
        return Ok(Json::Object(pairs));
    }
    loop {
        skip_ws(b, pos);
        let key = parse_string(b, pos)?;
        skip_ws(b, pos);
        if b.get(*pos) != Some(&b':') {
            return Err(ParseError);
        }
        *pos += 1;
        let value = parse_value(b, pos)?;
        pairs.push((key, value));
        skip_ws(b, pos);
        match b.get(*pos) {
            Some(b',') => *pos += 1,
            Some(b'}') => {
                *pos += 1;
                return Ok(Json::Object(pairs));
            }
            _ => return Err(ParseError),
        }
    }
}

/// Render as `jsonb` does: object keys sorted, the LAST of any duplicate pair
/// kept, `": "` after a key and `", "` between elements. Numbers keep their
/// original text, so `1.10` stays `1.10`.
pub fn render_jsonb(v: &Json) -> String {
    let mut out = String::new();
    write_jsonb(v, &mut out);
    out
}

fn write_jsonb(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Number(n) => out.push_str(&normalise_number(n)),
        Json::Str(s) => write_json_string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_jsonb(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            // Last duplicate wins, then sort. PostgreSQL sorts by key length
            // first and then bytewise, which is the order its internal
            // representation stores them in — NOT plain lexicographic.
            let mut kept: Vec<(String, &Json)> = Vec::new();
            for (k, val) in pairs {
                match kept.iter_mut().find(|(existing, _)| existing == k) {
                    Some(slot) => slot.1 = val,
                    None => kept.push((k.clone(), val)),
                }
            }
            kept.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
            out.push('{');
            for (i, (k, val)) in kept.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json_string(k, out);
                out.push_str(": ");
                write_jsonb(val, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A JSON number in `numeric`'s spelling.
///
/// `jsonb` stores numbers as `numeric`, so the exponent is expanded
/// (`-1.5e10` -> `-15000000000`) while a trailing zero written in the literal
/// survives (`1.10` stays `1.10`, because that is its scale). Both measured.
fn normalise_number(text: &str) -> String {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (text, 0),
    };
    let (neg, body) = match mantissa.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, mantissa),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let mut digits: String = format!("{int_part}{frac_part}");
    // Scale is how many digits sit after the point once the exponent is applied.
    let mut scale = frac_part.len() as i32 - exponent;
    if scale < 0 {
        // The exponent moves the point right past every digit: pad with zeros.
        digits.push_str(&"0".repeat((-scale) as usize));
        scale = 0;
    }
    let scale = scale as usize;
    if scale >= digits.len() {
        digits = format!("{}{}", "0".repeat(scale - digits.len() + 1), digits);
    }
    let split = digits.len() - scale;
    let (whole, frac) = digits.split_at(split);
    // A leading zero run is not part of the value, but one digit must remain.
    let whole_trimmed = whole.trim_start_matches('0');
    let whole = if whole_trimmed.is_empty() {
        "0"
    } else {
        whole_trimmed
    };
    let sign = if neg && !(whole == "0" && frac.chars().all(|c| c == '0')) {
        "-"
    } else {
        ""
    };
    if frac.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{frac}")
    }
}

/// Render a `json` value the way the `json` type does: the ORIGINAL text of
/// the value, not the normalised `jsonb` form.
///
/// `json` preserves whitespace, key order and duplicate keys; `jsonb` does
/// not. Both operators below hand back a document that has been through the
/// parser, so this is the closest a re-render gets -- key ORDER survives
/// (the parser keeps it), whitespace does not.
pub fn render_json(v: &Json) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(text) => out.push_str(text),
        Json::Str(s) => write_json_string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json_string(k, out);
                out.push_str(": ");
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

/// One step of `->` / `#>`: a member by NAME, or an array element by INDEX.
///
/// A negative index counts from the end (`-1` is the last element), which is
/// PostgreSQL's rule and not most JSON libraries'. Anything that does not
/// apply -- a name against an array, an index against an object, an index past
/// the end -- is `None`, which the operators answer as SQL NULL rather than an
/// error.
pub fn member<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        Json::Array(items) => {
            let index: i64 = key.parse().ok()?;
            let index = if index < 0 {
                items.len().checked_sub(index.unsigned_abs() as usize)?
            } else {
                index as usize
            };
            items.get(index)
        }
        _ => None,
    }
}

/// The `->>` reading of a value: a JSON string is its CONTENT, a JSON null is
/// SQL NULL, and everything else is its JSON text.
pub fn as_sql_text(value: &Json) -> Option<String> {
    match value {
        Json::Null => None,
        Json::Str(s) => Some(s.clone()),
        other => Some(render_json(other)),
    }
}

/// `jsonb ? key` -- an object has that KEY, an array contains that STRING, and
/// a scalar string IS that string.
pub fn contains_key(value: &Json, key: &str) -> bool {
    match value {
        Json::Object(pairs) => pairs.iter().any(|(k, _)| k == key),
        Json::Array(items) => items.iter().any(|i| matches!(i, Json::Str(s) if s == key)),
        Json::Str(s) => s == key,
        _ => false,
    }
}

/// `jsonb @> jsonb` -- containment, PostgreSQL's rules.
///
/// An object contains another when it has every one of its pairs; an array
/// contains another when it has every one of its elements; and a top-level
/// array contains a bare SCALAR that is one of its elements, which is the rule
/// people forget. Comparison is by VALUE, so key order and whitespace do not
/// matter.
pub fn contains(haystack: &Json, needle: &Json) -> bool {
    match (haystack, needle) {
        (Json::Object(hay), Json::Object(need)) => need
            .iter()
            .all(|(k, nv)| hay.iter().any(|(hk, hv)| hk == k && contains(hv, nv))),
        (Json::Array(hay), Json::Array(need)) => {
            need.iter().all(|nv| hay.iter().any(|hv| contains(hv, nv)))
        }
        // An array contains a scalar it holds.
        (Json::Array(hay), needle) => hay.iter().any(|hv| equal(hv, needle)),
        (a, b) => equal(a, b),
    }
}

/// Value equality, which for numbers is NUMERIC equality rather than text: the
/// parser keeps a number's original text, so `1` and `1.0` differ as strings
/// and are the same number.
pub fn equal(a: &Json, b: &Json) -> bool {
    match (a, b) {
        // NUMERIC equality, not text: `jsonb` keeps a number's scale, so `1.0`
        // and `1` render differently and are the same number -- which is what
        // `@>` compares.
        (Json::Number(x), Json::Number(y)) => {
            crate::compare_decimal_text(&normalise_number(x), &normalise_number(y))
                == Some(std::cmp::Ordering::Equal)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equal(a, b))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && equal(v, v2)))
        }
        _ => a == b,
    }
}

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

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not JSON at all.
    Syntax,
    /// A `\uD800`..`\uDFFF` escape with no partner: one half of a UTF-16
    /// pair, which decodes to no character. PostgreSQL's `json` type keeps
    /// the escape as written; `jsonb` rejects it (22P02).
    UnpairedSurrogate,
    /// `\u0000`: decodes to a character text cannot hold. `json` keeps the
    /// escape; `jsonb` rejects it with 22P05.
    NulEscape,
}

/// Parse a complete JSON document. Trailing non-whitespace is an error, so
/// `{"a":1} x` is rejected the way PostgreSQL rejects it.
pub fn parse(text: &str) -> Result<Json, ParseError> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let value = parse_value(bytes, &mut pos)?;
    skip_ws(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(ParseError::Syntax);
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
        return Err(ParseError::Syntax);
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
        return Err(ParseError::Syntax);
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
        return Err(ParseError::Syntax);
    }
    // A leading zero may not be followed by another digit: `01` is not JSON.
    if b[int_start] == b'0' && *pos - int_start > 1 {
        return Err(ParseError::Syntax);
    }
    if b.get(*pos) == Some(&b'.') {
        *pos += 1;
        let frac_start = *pos;
        while matches!(b.get(*pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
        if *pos == frac_start {
            return Err(ParseError::Syntax);
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
            return Err(ParseError::Syntax);
        }
    }
    let text = std::str::from_utf8(&b[start..*pos]).map_err(|_| ParseError::Syntax)?;
    Ok(Json::Number(text.to_string()))
}

/// The four hex digits after `\\u`, cursor advanced past them.
fn parse_hex4(b: &[u8], pos: &mut usize) -> Result<u32, ParseError> {
    let hex = b.get(*pos..*pos + 4).ok_or(ParseError::Syntax)?;
    let hex = std::str::from_utf8(hex).map_err(|_| ParseError::Syntax)?;
    if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(ParseError::Syntax);
    }
    let n = u32::from_str_radix(hex, 16).map_err(|_| ParseError::Syntax)?;
    *pos += 4;
    Ok(n)
}

fn parse_string(b: &[u8], pos: &mut usize) -> Result<String, ParseError> {
    if b.get(*pos) != Some(&b'"') {
        return Err(ParseError::Syntax);
    }
    *pos += 1;
    let mut out = String::new();
    loop {
        let Some(&c) = b.get(*pos) else {
            return Err(ParseError::Syntax);
        };
        match c {
            b'"' => {
                *pos += 1;
                return Ok(out);
            }
            b'\\' => {
                *pos += 1;
                let Some(&e) = b.get(*pos) else {
                    return Err(ParseError::Syntax);
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
                        let n = parse_hex4(b, pos)?;
                        // A surrogate pair is two escapes -- `\ud83d\ude00` is
                        // one character -- and either half on its own is no
                        // character at all.
                        let n = match n {
                            0xD800..=0xDBFF => {
                                if b.get(*pos..*pos + 2) != Some(b"\\u") {
                                    return Err(ParseError::UnpairedSurrogate);
                                }
                                *pos += 2;
                                let lo = parse_hex4(b, pos)?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return Err(ParseError::UnpairedSurrogate);
                                }
                                0x10000 + ((n - 0xD800) << 10) + (lo - 0xDC00)
                            }
                            0xDC00..=0xDFFF => return Err(ParseError::UnpairedSurrogate),
                            0 => return Err(ParseError::NulEscape),
                            n => n,
                        };
                        out.push(char::from_u32(n).ok_or(ParseError::Syntax)?);
                    }
                    _ => return Err(ParseError::Syntax),
                }
            }
            // A raw control character is not valid inside a JSON string.
            0x00..=0x1f => return Err(ParseError::Syntax),
            _ => {
                let rest = std::str::from_utf8(&b[*pos..]).map_err(|_| ParseError::Syntax)?;
                let ch = rest.chars().next().ok_or(ParseError::Syntax)?;
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
            _ => return Err(ParseError::Syntax),
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
            return Err(ParseError::Syntax);
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
            _ => return Err(ParseError::Syntax),
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

/// A `jsonb` value's IDENTITY: its normalised text with every number
/// reduced to its value (`1.0`, `1.00` and `1` alike), so two jsonb values are
/// equal exactly when their keys are -- PostgreSQL's jsonb equality compares
/// numbers as `numeric`s, scale ignored (15.19: `'{"x":1}' = '{"x":1.0}'`).
/// `None` when the text is not JSON.
pub fn jsonb_value_key(text: &str) -> Option<String> {
    let parsed = parse(text).ok()?;
    let mut out = String::new();
    write_value_key(&parsed, &mut out);
    Some(out)
}

fn write_value_key(v: &Json, out: &mut String) {
    match v {
        Json::Number(n) => {
            let t = normalise_number(n);
            let t = if t.contains('.') {
                t.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                t
            };
            out.push_str(if t == "-0" { "0" } else { &t });
        }
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value_key(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            let mut kept: Vec<(&String, &Json)> = Vec::new();
            for (k, val) in pairs {
                match kept.iter_mut().find(|(existing, _)| *existing == k) {
                    Some(slot) => slot.1 = val,
                    None => kept.push((k, val)),
                }
            }
            kept.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(b.0)));
            out.push('{');
            for (i, (k, val)) in kept.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_value_key(val, out);
            }
            out.push('}');
        }
        other => write_jsonb(other, out),
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

/// Render a `json` value with no whitespace, as `transform_json_string_values`
/// writes one (`ts_headline` over `json`): key order, duplicates and each
/// number's text kept.
pub fn render_json_compact(v: &Json) -> String {
    fn write(v: &Json, out: &mut String) {
        match v {
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Json::Object(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(k, out);
                    out.push(':');
                    write(v, out);
                }
                out.push('}');
            }
            scalar => write_json(scalar, out),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

/// A `json` value as its SOURCE text, with the structure needed to walk it.
///
/// `json` keeps its input exactly, and what its operators and functions hand
/// back of it is the input's own text: `'{"a": [1,  2]}'::json -> 'a'` is
/// `[1,  2]`, spacing and all. A [`Json`] tree cannot give that back, so the
/// `json` (not `jsonb`) extractions walk this instead.
pub struct Raw<'a> {
    /// The value's text, from its first character to its last.
    pub text: &'a str,
    pub kind: RawKind<'a>,
}

pub enum RawKind<'a> {
    Null,
    /// A string, unescaped.
    Str(String),
    /// A number or a boolean.
    Other,
    Array(Vec<Raw<'a>>),
    /// In input order, duplicates kept.
    Object(Vec<(String, Raw<'a>)>),
}

/// Walk valid JSON text into [`Raw`] spans. `None` when the text is not
/// JSON; callers [`parse`] it first, for the error.
pub fn parse_raw(text: &str) -> Option<Raw<'_>> {
    fn ws(b: &[u8], i: &mut usize) {
        while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            *i += 1;
        }
    }
    fn string(s: &str, i: &mut usize) -> Option<String> {
        let b = s.as_bytes();
        let start = *i;
        if b.get(*i) != Some(&b'"') {
            return None;
        }
        *i += 1;
        loop {
            match b.get(*i)? {
                b'\\' => *i += 2,
                b'"' => break,
                _ => *i += 1,
            }
        }
        *i += 1;
        match parse(&s[start..*i]).ok()? {
            Json::Str(v) => Some(v),
            _ => None,
        }
    }
    fn value<'a>(s: &'a str, i: &mut usize) -> Option<Raw<'a>> {
        let b = s.as_bytes();
        ws(b, i);
        let start = *i;
        let kind = match *b.get(*i)? {
            b'"' => RawKind::Str(string(s, i)?),
            open @ (b'[' | b'{') => {
                let close = if open == b'[' { b']' } else { b'}' };
                *i += 1;
                let mut items = Vec::new();
                let mut members = Vec::new();
                ws(b, i);
                if b.get(*i) == Some(&close) {
                    *i += 1;
                } else {
                    loop {
                        if open == b'{' {
                            ws(b, i);
                            let key = string(s, i)?;
                            ws(b, i);
                            if b.get(*i) != Some(&b':') {
                                return None;
                            }
                            *i += 1;
                            members.push((key, value(s, i)?));
                        } else {
                            items.push(value(s, i)?);
                        }
                        ws(b, i);
                        match b.get(*i)? {
                            b',' => *i += 1,
                            c if *c == close => {
                                *i += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
                if open == b'[' {
                    RawKind::Array(items)
                } else {
                    RawKind::Object(members)
                }
            }
            _ => {
                while !matches!(
                    b.get(*i),
                    None | Some(b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
                ) {
                    *i += 1;
                }
                if &s[start..*i] == "null" {
                    RawKind::Null
                } else {
                    RawKind::Other
                }
            }
        };
        Some(Raw {
            text: &s[start..*i],
            kind,
        })
    }
    let mut i = 0;
    let v = value(text, &mut i)?;
    ws(text.as_bytes(), &mut i);
    (i == text.len()).then_some(v)
}

impl<'a> Raw<'a> {
    /// One step of `->` / `#>` over `json`: as [`member`], except that a
    /// key given twice answers its LAST value, as `json`'s field lookup does
    /// (`'{"a":1,"a":2}'::json -> 'a'` is `2`).
    pub fn member(&self, key: &str) -> Option<&Raw<'a>> {
        match &self.kind {
            RawKind::Object(pairs) => pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            RawKind::Array(items) => {
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

    /// The `->>` reading: a string's content, NULL for null, and otherwise
    /// the value's own text.
    pub fn sql_text(&self) -> Option<String> {
        match &self.kind {
            RawKind::Null => None,
            RawKind::Str(s) => Some(s.clone()),
            _ => Some(self.text.to_string()),
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

/// Two `jsonb` values in PostgreSQL's btree order (`compareJsonbContainers`,
/// `jsonb_util.c`), or `None` when either text is not JSON.
///
/// The order walks both values token by token. At a scalar or at the start
/// of a container whose types differ, the type decides: null < string <
/// number < boolean < array < object. Two arrays compare their element
/// counts first and two objects their pair counts, then their contents in
/// stored order -- an object's keys sorted by length and then bytes, a
/// duplicate key keeping its last value. A top-level scalar is stored as a
/// one-element "raw scalar" array, which is why an empty array sorts below
/// every scalar: it is an array with fewer elements than the scalar's.
/// Strings compare under the database's collation (`C` here: by bytes) and
/// numbers as `numeric`s.
pub fn jsonb_compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (a, b) = (parse(a).ok()?, parse(b).ok()?);
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    jsonb_tokens(&a, true, &mut ta);
    jsonb_tokens(&b, true, &mut tb);
    for (x, y) in ta.iter().zip(&tb) {
        let order = token_compare(x, y);
        if order != std::cmp::Ordering::Equal {
            return Some(order);
        }
    }
    // Equal throughout: the counts compared at each container start make the
    // token lists the same length.
    Some(ta.len().cmp(&tb.len()))
}

/// One step of `JsonbIteratorNext` (without the container ends, which
/// compare equal and are skipped).
enum Token<'a> {
    Array {
        elements: usize,
        raw_scalar: bool,
    },
    Object {
        pairs: usize,
    },
    /// An object's key: a string.
    Key(&'a String),
    Scalar(&'a Json),
}

/// `JsonbValue.type`'s order: null, string, numeric, bool, array, object.
fn token_type(t: &Token<'_>) -> u8 {
    match t {
        Token::Scalar(Json::Null) => 0,
        Token::Key(_) | Token::Scalar(Json::Str(_)) => 1,
        Token::Scalar(Json::Number(_)) => 2,
        Token::Scalar(Json::Bool(_)) => 3,
        Token::Array { .. } | Token::Scalar(Json::Array(_)) => 0x10,
        Token::Object { .. } | Token::Scalar(Json::Object(_)) => 0x11,
    }
}

fn token_compare(x: &Token<'_>, y: &Token<'_>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (tx, ty) = (token_type(x), token_type(y));
    if tx != ty {
        return tx.cmp(&ty);
    }
    match (x, y) {
        (
            Token::Array {
                elements: ea,
                raw_scalar: ra,
            },
            Token::Array {
                elements: eb,
                raw_scalar: rb,
            },
        ) => {
            // Both checks run, the second overriding the first, as in
            // `compareJsonbContainers`: a raw scalar sorts below an array of
            // as many elements, and fewer elements sort first.
            let mut order = Ordering::Equal;
            if ra != rb {
                order = if *ra {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            if ea != eb {
                order = ea.cmp(eb);
            }
            order
        }
        (Token::Object { pairs: pa }, Token::Object { pairs: pb }) => pa.cmp(pb),
        _ => {
            let text = |t: &Token<'_>| -> Option<String> {
                match t {
                    Token::Key(k) => Some((*k).clone()),
                    Token::Scalar(Json::Str(s)) => Some(s.clone()),
                    _ => None,
                }
            };
            match (x, y) {
                (Token::Scalar(Json::Number(a)), Token::Scalar(Json::Number(b))) => {
                    crate::numeric::compare_decimal_text(&normalise_number(a), &normalise_number(b))
                        .unwrap_or(Ordering::Equal)
                }
                (Token::Scalar(Json::Bool(a)), Token::Scalar(Json::Bool(b))) => a.cmp(b),
                _ => match (text(x), text(y)) {
                    (Some(a), Some(b)) => a.as_bytes().cmp(b.as_bytes()),
                    _ => Ordering::Equal,
                },
            }
        }
    }
}

fn jsonb_tokens<'a>(v: &'a Json, top: bool, out: &mut Vec<Token<'a>>) {
    match v {
        Json::Array(items) => {
            out.push(Token::Array {
                elements: items.len(),
                raw_scalar: false,
            });
            for item in items {
                jsonb_tokens(item, false, out);
            }
        }
        Json::Object(pairs) => {
            let mut kept: Vec<(&String, &Json)> = Vec::new();
            for (k, val) in pairs {
                match kept.iter_mut().find(|(existing, _)| *existing == k) {
                    Some(slot) => slot.1 = val,
                    None => kept.push((k, val)),
                }
            }
            kept.sort_by(|x, y| x.0.len().cmp(&y.0.len()).then_with(|| x.0.cmp(y.0)));
            out.push(Token::Object { pairs: kept.len() });
            for (k, val) in kept {
                out.push(Token::Key(k));
                jsonb_tokens(val, false, out);
            }
        }
        scalar if top => {
            out.push(Token::Array {
                elements: 1,
                raw_scalar: true,
            });
            out.push(Token::Scalar(scalar));
        }
        scalar => out.push(Token::Scalar(scalar)),
    }
}

#[cfg(test)]
mod jsonb_order_tests {
    use super::jsonb_compare;

    /// PostgreSQL 15.19: `ORDER BY j` over these sixteen jsonb values.
    #[test]
    fn jsonb_orders_as_postgres() {
        let pg = [
            "[]",
            "null",
            "\"a\"",
            "\"b\"",
            "1",
            "2.5",
            "false",
            "true",
            "[null]",
            "[\"a\"]",
            "[1]",
            "[0,0]",
            "{}",
            "{\"a\":1}",
            "{\"b\":0}",
            "{\"a\":1,\"b\":2}",
        ];
        let mut ours = pg.to_vec();
        ours.reverse();
        ours.sort_by(|a, b| jsonb_compare(a, b).unwrap());
        assert_eq!(ours, pg);
        // Numbers compare as numerics, and keys as strings (15.19:
        // `'{"b":1}' > '{"aa":1}'` is true, though "aa" is STORED after "b").
        assert_eq!(jsonb_compare("1.0", "1"), Some(std::cmp::Ordering::Equal));
        assert_eq!(
            jsonb_compare("{\"b\":1}", "{\"aa\":1}"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(jsonb_compare("not json", "1"), None);
    }
}

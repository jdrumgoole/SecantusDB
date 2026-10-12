//! Built-in functions picked by their argument TYPES, which the name-keyed
//! tables in `scalar` cannot do: `substr(bytea, int)` is a different
//! function from `substr(text, int)`, and answered the text one's rendering
//! of the bytes until this module took it.
//!
//! Each was found by `tools/probes/pg_builtin_coverage.py`, which calls every
//! function in PostgreSQL 15's `pg_proc` on both servers, and each is
//! measured against PostgreSQL 15.19 in
//! `tools/probes/pg_corpora/builtin_coverage.sql`.
//!
//! [`result_type`] says whether a call is one of these, from the static
//! types of its arguments; [`call`] runs it. A call that is not falls
//! through to the planner's other paths untouched.

use bson::Bson;

use crate::{bytea, Error, Result};

fn intish(t: &str) -> bool {
    matches!(t, "int2" | "int4" | "int8" | "" | "unknown")
}

/// The result type of `name` over `types`, when this module answers it.
pub(crate) fn result_type(name: &str, types: &[String]) -> Option<String> {
    let t = |i: usize| types.get(i).map_or("", String::as_str);
    let bytea_first = t(0) == "bytea";
    let out = match (name, types.len()) {
        ("substr" | "substring", 2 | 3) if bytea_first && types[1..].iter().all(|t| intish(t)) => {
            "bytea"
        }
        ("overlay", 3 | 4) if bytea_first && types[2..].iter().all(|t| intish(t)) => "bytea",
        ("md5", 1) if bytea_first => "text",
        ("btrim" | "ltrim" | "rtrim", 2) if bytea_first => "bytea",
        ("length", 2) if bytea_first => "int4",
        ("get_bit", 2) if bytea_first => "int4",
        ("set_bit", 3) if bytea_first => "bytea",
        ("bit_count", 1) if bytea_first => "int8",
        ("regexp_match", 2 | 3) => "text[]",
        ("regexp_replace", 4) if matches!(t(3), "int2" | "int4" | "int8") => "text",
        ("regexp_replace", 5 | 6) => "text",
        ("parse_ident", 1 | 2) => "text[]",
        ("convert_to", 2) => "bytea",
        ("convert", 3) => "bytea",
        ("num_nulls" | "num_nonnulls", n) if n >= 1 => "int4",
        ("trunc", 1) if t(0) == "macaddr" => "macaddr",
        ("trunc", 1) if t(0) == "macaddr8" => "macaddr8",
        ("macaddr8_set7bit", 1) => "macaddr8",
        ("overlaps", 4) if types.iter().all(|t| period_type(t)) => "bool",
        ("to_tsvector", 1 | 2) if matches!(t(types.len() - 1), "json" | "jsonb") => "tsvector",
        ("multirange", 1) if crate::range::is_range_type(t(0)) => {
            return Some(crate::range::multirange_name_for(t(0)));
        }
        _ => return None,
    };
    Some(out.to_string())
}

/// A type one end of an `OVERLAPS` period may have.
fn period_type(t: &str) -> bool {
    matches!(
        t,
        "date" | "time" | "timetz" | "timestamp" | "timestamptz" | "interval" | "" | "unknown"
    ) || t == "text"
    // An untyped NULL is typed `text` where nothing says otherwise.
}

/// Does a NULL argument make the answer NULL without the function running?
fn strict(name: &str) -> bool {
    !matches!(name, "num_nulls" | "num_nonnulls" | "overlaps")
}

/// `(s1, e1) OVERLAPS (s2, e2)`, as PostgreSQL's `overlaps_timestamp`: an
/// end given as an interval is that long after its start, each period is put
/// in order, and a NULL end leaves the answer NULL only where it decides it.
/// Two periods that start together overlap, even when both are empty.
fn overlaps(args: &[Bson], types: &[String]) -> Result<Bson> {
    let greater = |a: &Bson, b: &Bson| -> Result<bool> {
        Ok(crate::eval_binary(">", a.clone(), b.clone())? == Bson::Boolean(true))
    };
    let period = |i: usize| -> Result<(Bson, Bson)> {
        // A date beside a timestamp or an interval is midnight of that day.
        let point = |v: &Bson, t: &str| -> Result<Bson> {
            if t == "date" && *v != Bson::Null {
                crate::cast_value(v.clone(), "timestamp")
            } else {
                Ok(v.clone())
            }
        };
        let start = point(&args[i], &types[i])?;
        let mut end = point(&args[i + 1], &types[i + 1])?;
        if types[i + 1] == "interval" && end != Bson::Null && start != Bson::Null {
            end = crate::eval_binary("+", start.clone(), end)?;
        }
        Ok(match (start == Bson::Null, end == Bson::Null) {
            // A period with no start is the instant at its end.
            (true, _) => (end, Bson::Null),
            (false, true) => (start, Bson::Null),
            (false, false) if greater(&start, &end)? => (end, start),
            (false, false) => (start, end),
        })
    };
    let (s1, e1) = period(0)?;
    let (s2, e2) = period(2)?;
    if s1 == Bson::Null || s2 == Bson::Null {
        return Ok(Bson::Null);
    }
    let (first_end, later_start, later_end) = if greater(&s1, &s2)? {
        (&e2, &s1, &e1)
    } else if greater(&s2, &s1)? {
        (&e1, &s2, &e2)
    } else {
        // They start together.
        return Ok(if e1 == Bson::Null || e2 == Bson::Null {
            Bson::Null
        } else {
            Bson::Boolean(true)
        });
    };
    if *first_end == Bson::Null {
        return Ok(Bson::Null);
    }
    if greater(first_end, later_start)? {
        return Ok(Bson::Boolean(true));
    }
    if *later_end == Bson::Null {
        return Ok(Bson::Null);
    }
    Ok(Bson::Boolean(false))
}

fn int(v: &Bson, what: &str) -> Result<i64> {
    match v {
        Bson::Int32(i) => Ok(i64::from(*i)),
        Bson::Int64(i) => Ok(*i),
        Bson::String(s) => s.trim().parse().map_err(|_| {
            Error::InvalidText(format!("invalid input syntax for type {what}: \"{s}\""))
        }),
        other => crate::value_text(other).trim().parse().map_err(|_| {
            Error::InvalidText(format!(
                "invalid input syntax for type {what}: \"{}\"",
                crate::value_text(other)
            ))
        }),
    }
}

fn text(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        other => crate::value_text(other),
    }
}

fn negative_length() -> Error {
    Error::Sqlstate("22011", "negative substring length not allowed".into())
}

/// `bytea_substr`: the bytes from 1-based `start`, `count` of them.
fn bytea_substr(bytes: &[u8], start: i64, count: Option<i64>) -> Result<Vec<u8>> {
    let first = start.max(1);
    let len = bytes.len() as i64;
    let take = match count {
        None => len,
        Some(c) if c < 0 => return Err(negative_length()),
        Some(c) => match start.checked_add(c) {
            // The end runs past any length: to the end.
            None => len,
            Some(end) if end < 1 => return Ok(Vec::new()),
            Some(end) => end - first,
        },
    };
    if first > len || take <= 0 {
        return Ok(Vec::new());
    }
    let from = (first - 1) as usize;
    let to = (from as i64 + take).min(len) as usize;
    Ok(bytes[from..to].to_vec())
}

/// The encodings a conversion here knows: UTF8, and the single-byte ones
/// whose every octet is its own code point.
enum Encoding {
    Utf8,
    Latin1,
    Ascii,
}

fn encoding(name: &str, role: &str) -> Result<(Encoding, String)> {
    let key = name.to_ascii_uppercase().replace(['-', '_', ' '], "");
    match key.as_str() {
        "UTF8" | "UNICODE" => Ok((Encoding::Utf8, "UTF8".into())),
        "LATIN1" | "ISO88591" => Ok((Encoding::Latin1, "LATIN1".into())),
        "SQLASCII" => Ok((Encoding::Ascii, "SQL_ASCII".into())),
        _ => Err(Error::Sqlstate(
            "22023",
            format!("invalid {role}encoding name \"{name}\""),
        )),
    }
}

fn invalid_utf8(bytes: &[u8], at: usize) -> Error {
    // PostgreSQL names the bytes of the sequence the leading byte announces.
    let lead = bytes[at];
    let want = if lead >= 0xF0 {
        4
    } else if lead >= 0xE0 {
        3
    } else if lead >= 0xC0 {
        2
    } else {
        1
    };
    let shown: Vec<String> = bytes[at..(at + want).min(bytes.len())]
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect();
    Error::Sqlstate(
        "22021",
        format!(
            "invalid byte sequence for encoding \"UTF8\": {}",
            shown.join(" ")
        ),
    )
}

fn decode(bytes: &[u8], from: &Encoding) -> Result<String> {
    match from {
        Encoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) if !s.contains('\0') => Ok(s.to_string()),
            Ok(_) => Err(Error::Sqlstate(
                "22021",
                "invalid byte sequence for encoding \"UTF8\": 0x00".into(),
            )),
            Err(e) => Err(invalid_utf8(bytes, e.valid_up_to())),
        },
        Encoding::Latin1 => Ok(bytes.iter().map(|b| *b as char).collect()),
        Encoding::Ascii => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn encode(s: &str, to: &Encoding, to_name: &str) -> Result<Vec<u8>> {
    match to {
        Encoding::Utf8 | Encoding::Ascii => Ok(s.as_bytes().to_vec()),
        Encoding::Latin1 => s
            .chars()
            .map(|c| {
                u8::try_from(u32::from(c)).map_err(|_| {
                    let mut buf = [0u8; 4];
                    let shown: Vec<String> = c
                        .encode_utf8(&mut buf)
                        .bytes()
                        .map(|b| format!("0x{b:02x}"))
                        .collect();
                    Error::Sqlstate(
                        "22P05",
                        format!(
                            "character with byte sequence {} in encoding \"UTF8\" has no \
                             equivalent in encoding \"{to_name}\"",
                            shown.join(" ")
                        ),
                    )
                })
            })
            .collect(),
    }
}

/// The options a regular-expression function takes, as PostgreSQL's
/// `parse_re_flags` reads them. `g` is the caller's to allow.
pub(crate) fn check_regex_flags(flags: &str) -> Result<()> {
    match flags.chars().find(|c| !"gbceimnpqstwx".contains(*c)) {
        Some(c) => Err(Error::Sqlstate(
            "22023",
            format!("invalid regular expression option: \"{c}\""),
        )),
        None => Ok(()),
    }
}

/// Where a regular-expression function keeps its options, by name and
/// argument count. `types` tells `regexp_replace`'s two four-argument forms
/// apart.
pub(crate) fn regex_flags_position(name: &str, types: &[String]) -> Option<usize> {
    let n = types.len();
    let at = match (name, n) {
        ("regexp_like" | "regexp_match" | "regexp_matches", 3) => 2,
        ("regexp_split_to_array" | "regexp_split_to_table", 3) => 2,
        ("regexp_count", 4) => 3,
        ("regexp_substr", 5 | 6) => 4,
        ("regexp_instr", 6 | 7) => 5,
        ("regexp_replace", 4) if !matches!(types[3].as_str(), "int2" | "int4" | "int8") => 3,
        ("regexp_replace", 6) => 5,
        _ => return None,
    };
    Some(at)
}

fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex> {
    regex::RegexBuilder::new(&crate::scalar::pg_regex_source(pattern))
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('n') || flags.contains('m') || flags.contains('w'))
        .dot_matches_new_line(!(flags.contains('n') || flags.contains('m') || flags.contains('p')))
        .ignore_whitespace(flags.contains('x'))
        .build()
        .map_err(|_| Error::InvalidRegex(format!("invalid regular expression: \"{pattern}\"")))
}

/// PostgreSQL's replacement text as the `regex` crate's: `\1` is a group,
/// `\&` the whole match, and a `$` is itself.
fn replacement_for(replacement: &str) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    out.push_str("${");
                    out.push(*d);
                    out.push('}');
                    chars.next();
                    continue;
                }
                Some('&') => {
                    out.push_str("${0}");
                    chars.next();
                    continue;
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                    continue;
                }
                _ => {}
            }
        } else if c == '$' {
            out.push_str("$$");
            continue;
        }
        out.push(c);
    }
    out
}

fn bad_parameter(name: &str, value: i64) -> Error {
    Error::Sqlstate(
        "22023",
        format!("invalid value for parameter \"{name}\": {value}"),
    )
}

/// `regexp_replace(source, pattern, replacement, start [, n [, flags]])`:
/// the `n`th match at or after character `start` is replaced, or every one
/// when `n` is 0. With `n` given, a `g` in the flags changes nothing.
fn regexp_replace_from(args: &[Bson]) -> Result<Bson> {
    let (source, pattern, replacement) = (text(&args[0]), text(&args[1]), text(&args[2]));
    let start = int(&args[3], "integer")?;
    if start < 1 {
        return Err(bad_parameter("start", start));
    }
    let n = match args.get(4) {
        Some(v) => {
            let n = int(v, "integer")?;
            if n < 0 {
                return Err(bad_parameter("n", n));
            }
            Some(n)
        }
        None => None,
    };
    let flags = args.get(5).map(text).unwrap_or_default();
    check_regex_flags(&flags)?;
    let re = build_regex(&pattern, &flags)?;
    let from = match source.char_indices().nth((start - 1) as usize) {
        Some((byte, _)) => byte,
        None => source.len(),
    };
    let replacement = replacement_for(&replacement);
    let (head, tail) = source.split_at(from);
    let which = match n {
        Some(n) => n,
        None if flags.contains('g') => 0,
        None => 1,
    };
    let mut out = String::from(head);
    let mut last = 0;
    for (i, caps) in re.captures_iter(tail).enumerate() {
        let m = caps.get(0).expect("group 0 is the match");
        if which != 0 && i as i64 + 1 != which {
            continue;
        }
        out.push_str(&tail[last..m.start()]);
        caps.expand(&replacement, &mut out);
        last = m.end();
        if which != 0 {
            break;
        }
    }
    out.push_str(&tail[last..]);
    Ok(Bson::String(out))
}

/// `regexp_match`: the first match's capture groups, or the whole match
/// when the pattern has none; NULL when nothing matches.
fn regexp_match(args: &[Bson]) -> Result<Bson> {
    let (source, pattern) = (text(&args[0]), text(&args[1]));
    let flags = args.get(2).map(text).unwrap_or_default();
    check_regex_flags(&flags)?;
    if flags.contains('g') {
        return Err(Error::Sqlstate(
            "22023",
            "regexp_match() does not support the \"global\" option".into(),
        ));
    }
    let re = build_regex(&pattern, &flags)?;
    let Some(caps) = re.captures(&source) else {
        return Ok(Bson::Null);
    };
    let group = |i: usize| {
        caps.get(i)
            .map_or(Bson::Null, |m| Bson::String(m.as_str().to_string()))
    };
    Ok(Bson::Array(if caps.len() == 1 {
        vec![group(0)]
    } else {
        (1..caps.len()).map(group).collect()
    }))
}

fn not_an_identifier(input: &str) -> Error {
    Error::Sqlstate(
        "22023",
        format!("string is not a valid identifier: \"{input}\""),
    )
}

/// `parse_ident`: a qualified name split into its parts, unquoted parts
/// folded to lower case. In strict mode anything after the name is an error.
fn parse_ident(input: &str, strict: bool) -> Result<Bson> {
    let chars: Vec<char> = input.chars().collect();
    let space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
    let start = |c: char| c == '_' || c.is_alphabetic() || !c.is_ascii();
    let cont = |c: char| start(c) || c.is_ascii_digit() || c == '$';
    let mut parts = Vec::new();
    let mut i = 0;
    while i < chars.len() && space(chars[i]) {
        i += 1;
    }
    let mut after_dot = false;
    loop {
        let mut missing = false;
        if chars.get(i) == Some(&'"') {
            i += 1;
            let mut part = String::new();
            loop {
                match chars.get(i) {
                    None => return Err(not_an_identifier(input)),
                    Some('"') if chars.get(i + 1) == Some(&'"') => {
                        part.push('"');
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(c) => {
                        part.push(*c);
                        i += 1;
                    }
                }
            }
            if part.is_empty() {
                return Err(not_an_identifier(input));
            }
            parts.push(Bson::String(part));
        } else if chars.get(i).is_some_and(|c| start(*c)) {
            let from = i;
            while chars.get(i).is_some_and(|c| cont(*c)) {
                i += 1;
            }
            let part: String = chars[from..i].iter().collect();
            parts.push(Bson::String(
                part.chars().map(crate::scalar::simple_lower).collect(),
            ));
        } else {
            missing = true;
        }
        if missing {
            // Nothing where a name must be: after a dot, or at the start.
            if after_dot || parts.is_empty() || strict {
                return Err(not_an_identifier(input));
            }
            break;
        }
        while i < chars.len() && space(chars[i]) {
            i += 1;
        }
        if chars.get(i) == Some(&'.') {
            after_dot = true;
            i += 1;
            while i < chars.len() && space(chars[i]) {
                i += 1;
            }
        } else if i >= chars.len() {
            break;
        } else {
            if strict {
                return Err(not_an_identifier(input));
            }
            break;
        }
    }
    Ok(Bson::Array(parts))
}

fn mac_bytes(v: &Bson) -> Vec<u8> {
    text(v)
        .split(':')
        .filter_map(|p| u8::from_str_radix(p, 16).ok())
        .collect()
}

fn mac_text(bytes: &[u8]) -> Bson {
    Bson::String(
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// Run `name` over `args`. Only called for what [`result_type`] claimed.
pub(crate) fn call(name: &str, args: &[Bson], types: &[String], variadic: bool) -> Result<Bson> {
    if strict(name) && args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    let bytes = |i: usize| bytea::parse(&args[i]);
    match name {
        "substr" | "substring" => {
            let count = args.get(2).map(|v| int(v, "integer")).transpose()?;
            Ok(bytea::to_binary(bytea_substr(
                &bytes(0)?,
                int(&args[1], "integer")?,
                count,
            )?))
        }
        "overlay" => {
            let (base, new) = (bytes(0)?, bytes(1)?);
            let at = int(&args[2], "integer")?;
            let len = match args.get(3) {
                Some(v) => int(v, "integer")?,
                None => new.len() as i64,
            };
            if at <= 0 {
                return Err(negative_length());
            }
            let after = at
                .checked_add(len)
                .ok_or_else(|| Error::NumericOutOfRange("integer out of range".into()))?;
            let mut out = bytea_substr(&base, 1, Some(at - 1))?;
            out.extend_from_slice(&new);
            out.extend(bytea_substr(&base, after, None)?);
            Ok(bytea::to_binary(out))
        }
        "md5" => Ok(Bson::String(crate::scalar::md5_hex(&bytes(0)?))),
        "btrim" | "ltrim" | "rtrim" => {
            let (value, set) = (bytes(0)?, bytes(1)?);
            let mut from = 0;
            let mut to = value.len();
            if name != "rtrim" {
                while from < to && set.contains(&value[from]) {
                    from += 1;
                }
            }
            if name != "ltrim" {
                while to > from && set.contains(&value[to - 1]) {
                    to -= 1;
                }
            }
            Ok(bytea::to_binary(value[from..to].to_vec()))
        }
        "length" => {
            let (enc, _) = encoding(&text(&args[1]), "")?;
            let value = bytes(0)?;
            Ok(Bson::Int32(match enc {
                Encoding::Utf8 => match std::str::from_utf8(&value) {
                    Ok(s) => s.chars().count() as i32,
                    Err(e) => return Err(invalid_utf8(&value, e.valid_up_to())),
                },
                _ => value.len() as i32,
            }))
        }
        "get_bit" | "set_bit" => {
            let value = bytes(0)?;
            let n = int(&args[1], "bigint")?;
            let bits = value.len() as i64 * 8;
            if n < 0 || n >= bits {
                return Err(Error::Sqlstate(
                    "2202E",
                    format!("index {n} out of valid range, 0..{}", bits - 1),
                ));
            }
            let (byte, bit) = ((n / 8) as usize, (n % 8) as u32);
            if name == "get_bit" {
                return Ok(Bson::Int32(i32::from((value[byte] >> bit) & 1)));
            }
            let new = int(&args[2], "integer")?;
            if new != 0 && new != 1 {
                return Err(Error::Sqlstate("22023", "new bit must be 0 or 1".into()));
            }
            let mut out = value;
            if new == 1 {
                out[byte] |= 1 << bit;
            } else {
                out[byte] &= !(1 << bit);
            }
            Ok(bytea::to_binary(out))
        }
        "bit_count" => Ok(Bson::Int64(
            bytes(0)?.iter().map(|b| i64::from(b.count_ones())).sum(),
        )),
        "regexp_match" => regexp_match(args),
        "regexp_replace" => regexp_replace_from(args),
        "parse_ident" => {
            let strict = !matches!(args.get(1), Some(Bson::Boolean(false)));
            parse_ident(&text(&args[0]), strict)
        }
        "convert_to" => {
            let (enc, shown) = encoding(&text(&args[1]), "destination ")?;
            Ok(bytea::to_binary(encode(&text(&args[0]), &enc, &shown)?))
        }
        "convert" => {
            let (from, _) = encoding(&text(&args[1]), "source ")?;
            let (to, shown) = encoding(&text(&args[2]), "destination ")?;
            Ok(bytea::to_binary(encode(
                &decode(&bytes(0)?, &from)?,
                &to,
                &shown,
            )?))
        }
        "num_nulls" | "num_nonnulls" => {
            // `VARIADIC array`: the array's elements are the arguments, and
            // a NULL array is a NULL answer.
            let items: Vec<&Bson> = match (variadic, args) {
                (true, [Bson::Null]) => return Ok(Bson::Null),
                (true, [one]) => match crate::arrays::strip(one) {
                    Bson::Array(_) => {
                        let Bson::Array(items) = crate::arrays::strip(one) else {
                            unreachable!()
                        };
                        let nulls = flat_nulls(&items);
                        let total = flat_len(&items);
                        return Ok(Bson::Int32(if name == "num_nulls" {
                            nulls
                        } else {
                            total - nulls
                        }));
                    }
                    _ => vec![one],
                },
                _ => args.iter().collect(),
            };
            let nulls = items.iter().filter(|v| ***v == Bson::Null).count() as i32;
            Ok(Bson::Int32(if name == "num_nulls" {
                nulls
            } else {
                items.len() as i32 - nulls
            }))
        }
        "trunc" => {
            let mut b = mac_bytes(&args[0]);
            for byte in b.iter_mut().skip(3) {
                *byte = 0;
            }
            Ok(mac_text(&b))
        }
        "macaddr8_set7bit" => {
            let mut b = mac_bytes(&args[0]);
            if let Some(first) = b.first_mut() {
                *first |= 0x02;
            }
            Ok(mac_text(&b))
        }
        "overlaps" => overlaps(args, types),
        "to_tsvector" => crate::fts::json_to_tsvector(args),
        "multirange" => {
            let range_type = types.first().map_or("", String::as_str);
            let range = crate::range::from_text(&text(&args[0]), range_type)?;
            Ok(Bson::String(crate::range::render_multirange(
                &crate::range::normalise_multirange(vec![range], range_type)?,
            )))
        }
        other => Err(Error::Unsupported(format!("function {other}"))),
    }
}

fn flat_len(items: &[Bson]) -> i32 {
    items
        .iter()
        .map(|i| match i {
            Bson::Array(inner) => flat_len(inner),
            _ => 1,
        })
        .sum()
}

fn flat_nulls(items: &[Bson]) -> i32 {
    items
        .iter()
        .map(|i| match i {
            Bson::Array(inner) => flat_nulls(inner),
            Bson::Null => 1,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    fn bytes(v: &[u8]) -> Bson {
        bytea::to_binary(v.to_vec())
    }

    #[test]
    fn the_bytea_overloads_are_claimed_and_the_text_ones_are_not() {
        assert_eq!(
            result_type("substr", &types(&["bytea", "int4"])).as_deref(),
            Some("bytea")
        );
        assert_eq!(result_type("substr", &types(&["text", "int4"])), None);
        assert_eq!(
            result_type("md5", &types(&["bytea"])).as_deref(),
            Some("text")
        );
        assert_eq!(result_type("md5", &types(&["text"])), None);
        // `regexp_replace(s, p, r, flags)` stays where it was; a start
        // position is this module's.
        assert_eq!(result_type("regexp_replace", &types(&["text"; 4])), None);
        assert_eq!(
            result_type("regexp_replace", &types(&["text", "text", "text", "int4"])).as_deref(),
            Some("text")
        );
        assert_eq!(
            result_type("multirange", &types(&["int4range"])).as_deref(),
            Some("int4multirange")
        );
    }

    #[test]
    fn bytea_substr_counts_from_one_and_refuses_a_negative_length() {
        let b = [1u8, 2, 3, 4, 5];
        assert_eq!(bytea_substr(&b, 2, None).unwrap(), vec![2, 3, 4, 5]);
        assert_eq!(bytea_substr(&b, 2, Some(2)).unwrap(), vec![2, 3]);
        // A start before the first byte uses up length before it.
        assert_eq!(bytea_substr(&b, -1, Some(3)).unwrap(), vec![1]);
        assert_eq!(bytea_substr(&b, 9, None).unwrap(), Vec::<u8>::new());
        assert!(bytea_substr(&b, 2, Some(-1)).is_err());
        assert_eq!(
            bytea_substr(&b, 2, Some(i64::MAX)).unwrap(),
            vec![2, 3, 4, 5]
        );
    }

    #[test]
    fn bits_are_counted_from_the_low_end_of_each_byte() {
        let t = types(&["bytea", "int8"]);
        let get = |n: i64| call("get_bit", &[bytes(&[0x61]), Bson::Int64(n)], &t, false);
        assert_eq!(get(0).unwrap(), Bson::Int32(1));
        assert_eq!(get(1).unwrap(), Bson::Int32(0));
        assert_eq!(get(5).unwrap(), Bson::Int32(1));
        assert!(get(8).is_err());
        assert_eq!(
            call(
                "bit_count",
                &[bytes(&[0xff, 0x01])],
                &types(&["bytea"]),
                false
            )
            .unwrap(),
            Bson::Int64(9)
        );
    }

    #[test]
    fn a_letter_that_is_no_regex_option_is_refused() {
        assert!(check_regex_flags("gi").is_ok());
        assert!(check_regex_flags("").is_ok());
        assert!(check_regex_flags("z").is_err());
        assert_eq!(
            regex_flags_position("regexp_like", &types(&["text"; 3])),
            Some(2)
        );
        assert_eq!(
            regex_flags_position("regexp_like", &types(&["text"; 2])),
            None
        );
        // The fourth argument of regexp_replace is its options, or a start.
        assert_eq!(
            regex_flags_position("regexp_replace", &types(&["text"; 4])),
            Some(3)
        );
        assert_eq!(
            regex_flags_position("regexp_replace", &types(&["text", "text", "text", "int4"])),
            None
        );
    }

    #[test]
    fn regexp_replace_from_a_start_replaces_the_nth_match() {
        let t = types(&["text", "text", "text", "int4", "int4"]);
        let run = |start: i32, n: i32| {
            let args = [
                Bson::String("abcabcabc".into()),
                Bson::String("b".into()),
                Bson::String("X".into()),
                Bson::Int32(start),
                Bson::Int32(n),
            ];
            match call("regexp_replace", &args, &t, false) {
                Ok(Bson::String(s)) => s,
                other => format!("{other:?}"),
            }
        };
        assert_eq!(run(1, 1), "aXcabcabc");
        assert_eq!(run(1, 2), "abcaXcabc");
        assert_eq!(run(1, 0), "aXcaXcaXc");
        assert_eq!(run(4, 1), "abcaXcabc");
        assert_eq!(run(1, 9), "abcabcabc");
    }

    #[test]
    fn parse_ident_folds_what_is_not_quoted() {
        let parts = |s: &str, strict: bool| match parse_ident(s, strict) {
            Ok(Bson::Array(items)) => items.iter().map(text).collect::<Vec<_>>().join("|"),
            other => format!("{other:?}"),
        };
        assert_eq!(parts("SomeSchema.someTable", true), "someschema|sometable");
        assert_eq!(parts("\"A b\".C", true), "A b|c");
        assert_eq!(parts("\"a\"\"b\"", true), "a\"b");
        assert_eq!(parts("a.b()", false), "a|b");
        assert!(parse_ident("a.b()", true).is_err());
        assert!(parse_ident("a..b", true).is_err());
        assert!(parse_ident("", false).is_err());
        assert!(parse_ident("1a", true).is_err());
    }

    #[test]
    fn num_nulls_counts_its_arguments_or_a_variadic_arrays_elements() {
        let args = [Bson::Int32(1), Bson::Null, Bson::Null];
        let t = types(&["int4", "unknown", "unknown"]);
        assert_eq!(call("num_nulls", &args, &t, false).unwrap(), Bson::Int32(2));
        assert_eq!(
            call("num_nonnulls", &args, &t, false).unwrap(),
            Bson::Int32(1)
        );
        let array = [Bson::Array(vec![
            Bson::Int32(1),
            Bson::Null,
            Bson::Int32(3),
        ])];
        let t = types(&["int4[]"]);
        assert_eq!(call("num_nulls", &array, &t, true).unwrap(), Bson::Int32(1));
        assert_eq!(
            call("num_nulls", &[Bson::Null], &t, true).unwrap(),
            Bson::Null
        );
    }

    #[test]
    fn a_character_latin1_cannot_hold_is_an_error() {
        let (latin1, name) = encoding("LATIN1", "").unwrap();
        assert_eq!(encode("é", &latin1, &name).unwrap(), vec![0xe9]);
        assert!(encode("€", &latin1, &name).is_err());
        assert!(encoding("nosuch", "destination ").is_err());
        assert!(decode(&[0xff], &Encoding::Utf8).is_err());
    }
}

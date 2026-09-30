//! Scalar built-in functions.
//!
//! Types matter as much as values here. PostgreSQL's `length` answers `int4`,
//! its `exp` answers `float8`, and `abs` gives back whatever it was handed —
//! and the two rounding families disagree, exactly as they do in casts:
//! `round` on a `numeric` goes half AWAY FROM ZERO while on a `float8` it goes
//! half TO EVEN. Every case here was measured against PostgreSQL 14 rather
//! than assumed.

use crate::{compare_constants, decimal_arith, parse_numeric, Error, Result};
use bson::Bson;

/// Is this a scalar built-in this server implements?
pub fn is_scalar(name: &str) -> bool {
    name == "secantus_hash_partition"
        || SCALAR_NAMES.contains(&name)
        || CATALOG_NAMES.contains(&name)
        || crate::arrays::is_array_function(name)
        || extension_scalar(name).is_some()
        || crate::fts::is_function(name)
        || crate::xml::is_function(name)
        || crate::jsonops::FUNCTIONS.contains(&name)
        || crate::mathfn::FUNCTIONS.contains(&name)
        || crate::pgcrypto::is_function(name)
        || crate::trgm::is_function(name)
        || crate::geom::FUNCTIONS.contains(&name)
}

/// Does this built-in's result type follow from its NAME alone?
///
/// Most do, and the planner uses that to type a call it has not run. The array
/// functions that answer an array do not: `array_remove(x, 1)` is whatever
/// `x` is, so they are typed from the CALL by `static_type` instead, and the
/// select-list fast path that only has a name has to step around them.
pub fn has_static_result_type(name: &str) -> bool {
    !crate::arrays::is_array_function(name) || crate::arrays::static_result_type(name).is_some()
}

/// The catalog functions that must be answered by the CONNECTION when they
/// stand alone, rather than folded here.
///
/// Each has a `ConstCol` of its own, and the server's value is the live one:
/// `current_setting` has to see a `set_config` from earlier in the session,
/// and folding it at plan time also means a caller with no session installed
/// (every planner unit test) gets "unrecognized configuration parameter" for
/// a GUC that exists. They are still reachable HERE, which is what makes
/// `current_setting('x') ~ '...'` work -- the expression path has no
/// `ConstCol` to defer to.
pub fn defers_to_connection(name: &str) -> bool {
    matches!(
        name,
        "current_database" | "current_catalog" | "current_setting"
    )
}

/// The catalog-facing built-ins, which a client calls inside an EXPRESSION at
/// least as often as on its own -- `version() LIKE 'PostgreSQL%'`,
/// `current_setting('x') ~ '...'`, `obj_description(oid) IS NULL`.
///
/// Kept apart from `SCALAR_NAMES` because they read SESSION state rather than
/// only their arguments, and because that is the fact worth seeing at a
/// glance: as a bare select-list target they become a `ConstCol` the server
/// resolves, and this list is what lets the constant evaluator reach them too.
/// Is `name` one of the catalog readers (`version`, `format_type`,
/// `obj_description` ...), which take this server's catalog columns?
pub(crate) fn is_catalog_reader(name: &str) -> bool {
    CATALOG_NAMES.contains(&name)
}

const CATALOG_NAMES: &[&str] = &[
    "version",
    "current_schema",
    "current_database",
    "current_catalog",
    "current_setting",
    "format_type",
    "obj_description",
    "col_description",
    "shobj_description",
    "pg_get_expr",
    "pg_table_is_visible",
    "pg_relation_is_publishable",
    "pg_get_userbyid",
    "pg_get_function_sqlbody",
    "pg_type_is_visible",
    "pg_function_is_visible",
    "pg_operator_is_visible",
    "pg_opclass_is_visible",
    "pg_opfamily_is_visible",
    "pg_collation_is_visible",
    "pg_conversion_is_visible",
    "pg_statistics_obj_is_visible",
    "pg_ts_config_is_visible",
    "pg_ts_dict_is_visible",
    "pg_ts_parser_is_visible",
    "pg_ts_template_is_visible",
];

/// The extension a function belongs to, when that extension's type is
/// installed. Without the extension PostgreSQL has no such function, and
/// neither does this server.
fn extension_scalar(name: &str) -> Option<crate::ExtensionType> {
    let owner = match name {
        "st_geomfromgeojson" | "st_srid" | "st_astext" | "st_asewkt" | "st_geomfromtext"
        | "st_geomfromewkt" => crate::ExtensionType::Geometry,
        "hstore" | "akeys" | "avals" | "skeys" | "svals" | "exist" | "defined"
        | "hstore_to_json" | "hstore_to_jsonb" | "delete" => crate::ExtensionType::Hstore,
        _ => return None,
    };
    let installed = match owner {
        crate::ExtensionType::Geometry => crate::extension_type("geometry"),
        crate::ExtensionType::Hstore => crate::extension_type("hstore"),
    };
    installed.filter(|i| *i == owner)
}

const SCALAR_NAMES: &[&str] = &[
    "pg_size_pretty",
    "pg_size_bytes",
    "pg_column_size",
    "to_char",
    "to_number",
    "similar_to_escape",
    "similar_escape",
    "jsonb_path_exists",
    "jsonb_path_match",
    "jsonb_path_query_first",
    "jsonb_path_query_array",
    "jsonb_path_exists_tz",
    "jsonb_path_match_tz",
    "jsonb_path_query_first_tz",
    "jsonb_path_query_array_tz",
    "gen_random_uuid",
    "uuid_generate_v4",
    "random",
    "upper",
    "lower",
    "initcap",
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "bit_length",
    "btrim",
    "trim",
    "ltrim",
    "rtrim",
    "substr",
    "substring",
    "replace",
    "repeat",
    "reverse",
    "left",
    "right",
    "strpos",
    "position",
    "concat",
    "concat_ws",
    "md5",
    "sha224",
    "sha256",
    "sha384",
    "sha512",
    "quote_ident",
    "format",
    "chr",
    "ascii",
    "split_part",
    "starts_with",
    "lpad",
    "rpad",
    "to_hex",
    "translate",
    "overlay",
    "quote_literal",
    "quote_nullable",
    "normalize",
    "is_normalized",
    "regexp_count",
    "regexp_instr",
    "regexp_substr",
    "regexp_like",
    "regexp_split_to_array",
    "unistr",
    "convert_from",
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "sqrt",
    "exp",
    "ln",
    "log",
    "log10",
    "power",
    "pow",
    "scale",
    "numeric_send",
    "min_scale",
    "trim_scale",
    "mod",
    "sign",
    "div",
    "greatest",
    "least",
    "get_byte",
    "set_byte",
    "encode",
    "decode",
    "now",
    // The planner's own: an inet / cidr value's order key (`enum_order`).
    "__net_sortkey",
    "__net_op",
    "__net_arith",
    "__net_diff",
    "__coll_key",
    "__coll_keyv",
    "__coll_value",
    "transaction_timestamp",
    "statement_timestamp",
    "clock_timestamp",
];

thread_local! {
    /// `(transaction start, statement start)` in Unix microseconds, as the
    /// executor installs them per statement (`set_clocks`).
    static CLOCKS: std::cell::Cell<Option<(i64, i64)>> = const { std::cell::Cell::new(None) };
}

/// Install the transaction's and the statement's start for the statements
/// that follow on this thread.
pub fn set_clocks(transaction_start: i64, statement_start: i64) {
    CLOCKS.with(|c| c.set(Some((transaction_start, statement_start))));
}

/// The wall clock, in Unix microseconds.
pub fn wall_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// `now()`: the TRANSACTION's start, as PostgreSQL has it -- also
/// `transaction_timestamp()`, `CURRENT_TIMESTAMP` and the `'now'` literal,
/// so two reads in one transaction agree. Before the executor installed a
/// clock, the wall clock.
pub fn now_value() -> Bson {
    let micros = CLOCKS
        .with(|c| c.get())
        .map_or_else(wall_micros, |(t, _)| t);
    crate::timestamptz_value_from_micros(micros)
}

/// `statement_timestamp()`: the statement's start.
fn statement_value() -> Bson {
    let micros = CLOCKS
        .with(|c| c.get())
        .map_or_else(wall_micros, |(_, s)| s);
    crate::timestamptz_value_from_micros(micros)
}

/// `clock_timestamp()`: the wall clock, which moves within a statement.
fn clock_value() -> Bson {
    crate::timestamptz_value_from_micros(wall_micros())
}

pub(crate) fn text(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(d) => d.to_string(),
        Bson::Decimal128(d) => crate::plain_numeric_text(&d.to_string()),
        v if crate::is_wide_numeric(v) => crate::numeric_text(v).unwrap_or_default(),
        // A boolean's text form is `t` / `f`: `concat(true)` is `t`
        // (measured), never `true`.
        Bson::Boolean(b) => (if *b { "t" } else { "f" }).to_string(),
        other => format!("{other:?}"),
    }
}

fn as_f64(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(i) => Some(f64::from(*i)),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        v if crate::is_wide_numeric(v) => crate::numeric_text(v).and_then(|t| t.parse().ok()),
        _ => None,
    }
}

fn as_i64(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn wrong_args(name: &str) -> Error {
    Error::Parse(format!(
        "function {name} does not exist with that argument list"
    ))
}

/// A one-based, clamped slice of a string by CHARACTERS, which is how
/// PostgreSQL's `substring` counts: a start before 1 does not shift the text,
/// it consumes part of the requested length.
fn substring(s: &str, start: i64, len: Option<i64>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let end = match len {
        Some(l) => start.saturating_add(l),
        None => i64::MAX,
    };
    chars
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let pos = *i as i64 + 1;
            pos >= start && (len.is_none() || pos < end)
        })
        .map(|(_, c)| *c)
        .collect()
}

/// `substring(s FROM pattern [FOR escape])`.
///
/// Without an escape the pattern is a POSIX regex and the answer is the first
/// CAPTURE GROUP when there is one, the whole match otherwise -- so
/// A PostgreSQL regular expression's POSIX character classes as the server's
/// `C.UTF-8` ctype reads them. The regex crate's `[[:alpha:]]` is ASCII-only,
/// where PostgreSQL under a UTF-8 locale counts `é` as a letter; `[:digit:]`
/// stays ASCII, as PostgreSQL's does (measured on 14 under `C.UTF-8`).
pub(crate) fn pg_regex_source(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut in_bracket = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            out.push(c);
            if let Some(n) = chars.get(i + 1) {
                out.push(*n);
            }
            i += 2;
            continue;
        }
        if in_bracket && c == '[' && chars.get(i + 1) == Some(&':') {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(end) = rest.find(":]") {
                let class = &rest[..end];
                let mapped = match class {
                    "alpha" => Some("\\p{Alphabetic}"),
                    "upper" => Some("\\p{Uppercase}"),
                    "lower" => Some("\\p{Lowercase}"),
                    "alnum" => Some("\\p{Alphabetic}0-9"),
                    _ => None,
                };
                if let Some(m) = mapped {
                    out.push_str(m);
                    i += 2 + class.chars().count() + 2;
                    continue;
                }
            }
        }
        if !in_bracket && c == '[' {
            in_bracket = true;
            out.push(c);
            i += 1;
            // A leading `^` and a leading `]` belong to the bracket.
            if chars.get(i) == Some(&'^') {
                out.push('^');
                i += 1;
            }
            if chars.get(i) == Some(&']') {
                out.push(']');
                i += 1;
            }
            continue;
        }
        if in_bracket && c == ']' {
            in_bracket = false;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// PostgreSQL 15's `regexp_count` / `regexp_instr` / `regexp_substr` /
/// `regexp_like`, over CHARACTER positions (1-based, as PostgreSQL counts).
/// Measured against PostgreSQL 15.19: an empty match counts at every
/// position, `g` is refused, and out-of-range parameters are 22023.
fn regexp_function(name: &str, args: &[Bson]) -> Result<Bson> {
    let text = |i: usize| -> Option<String> {
        args.get(i).map(|v| match v {
            Bson::String(s) => s.clone(),
            other => crate::value_text(other),
        })
    };
    let int = |i: usize, param: &str, default: i64, min: i64| -> Result<i64> {
        let Some(v) = args.get(i) else {
            return Ok(default);
        };
        let n = match v {
            Bson::Int32(n) => i64::from(*n),
            Bson::Int64(n) => *n,
            Bson::String(s) => s.trim().parse().map_err(|_| {
                Error::InvalidText(format!("invalid input syntax for type integer: \"{s}\""))
            })?,
            other => crate::value_text(other).parse().unwrap_or(default),
        };
        if n < min {
            return Err(Error::Sqlstate(
                "22023",
                format!("invalid value for parameter \"{param}\": {n}"),
            ));
        }
        Ok(n)
    };
    let (subject, pattern) = (text(0).unwrap_or_default(), text(1).unwrap_or_default());
    // Where each function keeps its flags.
    let flags_at = match name {
        "regexp_count" => 3,
        "regexp_like" => 2,
        "regexp_instr" => 5,
        _ => 4,
    };
    let flags = text(flags_at).unwrap_or_default();
    if flags.contains('g') {
        return Err(Error::Sqlstate(
            "22023",
            format!("{name}() does not support the \"global\" option"),
        ));
    }
    let re = regex::RegexBuilder::new(&pg_regex_source(&pattern))
        .case_insensitive(flags.contains('i'))
        .build()
        .map_err(|e| Error::InvalidRegex(format!("invalid regular expression: {e}")))?;
    if name == "regexp_like" {
        return Ok(Bson::Boolean(re.is_match(&subject)));
    }
    let start = if name == "regexp_like" {
        1
    } else {
        int(2, "start", 1, 1)?
    };
    // The byte offset of character `start`; past the end, no match.
    let chars: Vec<(usize, char)> = subject.char_indices().collect();
    let char_at = |byte: usize| {
        chars
            .iter()
            .position(|(b, _)| *b >= byte)
            .unwrap_or(chars.len())
    };
    let from = match chars.get(start as usize - 1) {
        Some((b, _)) => *b,
        None if start as usize - 1 == chars.len() => subject.len(),
        None => {
            return Ok(match name {
                "regexp_substr" => Bson::Null,
                _ => Bson::Int32(0),
            })
        }
    };
    if name == "regexp_count" {
        let n = re.find_iter(&subject[from..]).count();
        return Ok(Bson::Int32(i32::try_from(n).unwrap_or(i32::MAX)));
    }
    let nth = int(3, "n", 1, 1)?;
    let (endoption, subexpr) = if name == "regexp_instr" {
        let e = int(4, "endoption", 0, 0)?;
        if e > 1 {
            return Err(Error::Sqlstate(
                "22023",
                format!("invalid value for parameter \"endoption\": {e}"),
            ));
        }
        (e, int(6, "subexpr", 0, 0)?)
    } else {
        (0, int(5, "subexpr", 0, 0)?)
    };
    let hit = re.captures_iter(&subject[from..]).nth(nth as usize - 1);
    let group = hit.as_ref().and_then(|c| c.get(subexpr as usize));
    Ok(match (name, group) {
        ("regexp_substr", Some(m)) => Bson::String(m.as_str().to_string()),
        ("regexp_substr", None) => Bson::Null,
        (_, Some(m)) => {
            let byte = from + if endoption == 1 { m.end() } else { m.start() };
            Bson::Int32(char_at(byte) as i32 + 1)
        }
        (_, None) => Bson::Int32(0),
    })
}

/// `substring('abc' from '(b)')` and `substring('abc' from 'b')` both give
/// `b`, by different routes. No match is NULL, not the empty string.
///
/// With an escape it is the SQL-standard form: `%` and `_` are the LIKE
/// wildcards and the escape character doubled around `"` marks the part to
/// return, so `substring('abcde' from '%#"c#"%' for '#')` is `c`.
fn substring_pattern(subject: &str, pattern: &str, escape: Option<String>) -> Result<Bson> {
    let source = match escape.as_deref() {
        None => pattern.to_string(),
        Some(esc) => sql_substring_to_regex(pattern, esc)?,
    };
    let re = regex::Regex::new(&pg_regex_source(&source))
        .map_err(|e| Error::InvalidText(format!("invalid regular expression: {e}")))?;
    let Some(caps) = re.captures(subject) else {
        return Ok(Bson::Null);
    };
    let found = match caps.len() {
        1 => caps.get(0),
        _ => caps.get(1),
    };
    Ok(match found {
        Some(m) => Bson::String(m.as_str().to_string()),
        None => Bson::Null,
    })
}

/// The SQL-standard `substring ... for <escape>` pattern as a regex.
///
/// `%` is `.*`, `_` is `.`, `<esc>"` opens and closes the returned part, and
/// `<esc><char>` is that character literally. Everything else is escaped, so a
/// pattern cannot smuggle regex syntax through.
fn sql_substring_to_regex(pattern: &str, escape: &str) -> Result<String> {
    let esc = escape
        .chars()
        .next()
        .ok_or_else(|| Error::InvalidText("invalid escape string".to_string()))?;
    let mut out = String::from("^");
    let mut chars = pattern.chars().peekable();
    let mut groups = 0;
    while let Some(c) = chars.next() {
        if c == esc {
            match chars.next() {
                Some('"') => {
                    out.push_str(if groups == 0 { "(" } else { ")" });
                    groups += 1;
                }
                Some(other) => out.push_str(&regex::escape(&other.to_string())),
                None => return Err(Error::InvalidText("invalid escape string".to_string())),
            }
            continue;
        }
        match c {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    Ok(out)
}

/// `quote_literal(x)` -- the text as a SQL string literal.
///
/// A backslash forces the `E'...'` form, in which backslashes are doubled;
/// otherwise a plain `'...'` with every quote doubled. Measured on PostgreSQL
/// 14.13: `quote_literal('a\b')` is `E'a\\b'` and `quote_literal("it's")`
/// is `'it''s'`.
fn quote_literal_text(v: &str) -> String {
    if v.contains('\\') {
        format!("E'{}'", v.replace('\\', "\\\\").replace('\'', "''"))
    } else {
        format!("'{}'", v.replace('\'', "''"))
    }
}

/// `unistr(text)` -- decode PostgreSQL's Unicode escapes.
///
/// Four spellings, and the HINT PostgreSQL prints for a bad one names all of
/// them: `\XXXX`, `\+XXXXXX`, `\uXXXX`, `\UXXXXXXXX`. A doubled backslash is
/// a literal backslash. Anything else is `42601 invalid Unicode escape`.
fn unistr(input: &str) -> Result<String> {
    let bad = || Error::Parse("invalid Unicode escape".to_string());
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\\' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let next = *chars.get(i + 1).ok_or_else(bad)?;
        let (digits, skip) = match next {
            '\\' => {
                out.push('\\');
                i += 2;
                continue;
            }
            '+' => (6, 2),
            'u' => (4, 2),
            'U' => (8, 2),
            c if c.is_ascii_hexdigit() => (4, 1),
            _ => return Err(bad()),
        };
        let start = i + skip;
        let end = start + digits;
        if end > chars.len() {
            return Err(bad());
        }
        let hex: String = chars[start..end].iter().collect();
        let code = u32::from_str_radix(&hex, 16).map_err(|_| bad())?;
        out.push(char::from_u32(code).ok_or_else(bad)?);
        i = end;
    }
    Ok(out)
}

/// Evaluate a scalar built-in. `None` means "not one of ours".
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_scalar(name) {
        return None;
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    // Most scalar built-ins are NULL-propagating: given a NULL argument the
    // answer is NULL, not an error. Four are not, and all four IGNORE a NULL
    // argument instead: `concat` and `concat_ws` skip them, and `greatest` /
    // `least` pick the extreme of what remains, so `greatest(1, NULL)` is 1.
    // `format_type(oid, NULL)` is NOT null-propagating in its SECOND
    // argument: a NULL typmod means "no modifier", and PostgreSQL answers
    // `integer` rather than NULL.
    if name == "format_type" {
        return format_type_call(args);
    }
    // A hash partition's condition: NULL keys are part of the hash.
    if name == "secantus_hash_partition" {
        return crate::hashpart::satisfies(args);
    }
    // The array built-ins each decide what a NULL argument means -- see
    // `arrays`' header -- so they are routed BEFORE the blanket guard below.
    if crate::arrays::is_array_function(name) {
        return crate::arrays::call(name, args);
    }
    // Every other built-in sees an array's elements, not its lower bounds.
    if args.iter().any(crate::arrays::is_bounded) {
        let plain: Vec<Bson> = args.iter().map(crate::arrays::strip).collect();
        // `greatest` / `least` answer one of their ARGUMENTS, bounds and all:
        // compared by their elements, returned as given.
        if matches!(name, "greatest" | "least") {
            return eval(name, &plain).map(|out| {
                plain
                    .iter()
                    .position(|p| *p == out)
                    .map_or(out, |i| args[i].clone())
            });
        }
        return eval(name, &plain);
    }
    if let Some(out) = crate::fts::call(name, args) {
        return out;
    }
    if let Some(out) = crate::xml::call(name, args) {
        return out;
    }
    if let Some(out) = crate::jsonops::call(name, args) {
        return out;
    }
    if let Some(out) = crate::mathfn::call(name, args) {
        return out;
    }
    if crate::trgm::is_function(name) {
        return crate::trgm::call(name, args);
    }
    if let Some(out) = crate::pgcrypto::call(name, args) {
        return out;
    }
    if let Some(out) = crate::geom::call(name, args) {
        return out;
    }
    if crate::jsonpath::is_function(name) {
        return crate::jsonpath_call(name, args);
    }
    if !matches!(
        name,
        "concat" | "concat_ws" | "greatest" | "least" | "format" | "quote_nullable"
    ) && args.iter().any(|a| a == &Bson::Null)
    {
        return Ok(Bson::Null);
    }
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Bson::Null);
    let s = |i: usize| text(&arg(i));
    let need = |n: usize| -> Result<()> {
        if args.len() == n {
            Ok(())
        } else {
            Err(wrong_args(name))
        }
    };

    match name {
        "now" | "transaction_timestamp" => {
            need(0)?;
            Ok(now_value())
        }
        "__net_op" => {
            need(3)?;
            match (arg(1), arg(2)) {
                (Bson::Null, _) | (_, Bson::Null) => Ok(Bson::Null),
                (l, r) => crate::net::containment(&s(0), &text(&l), &text(&r))
                    .unwrap_or_else(|| Err(Error::Internal("not a network operator".into())))
                    .map(Bson::Boolean),
            }
        }
        "__net_arith" => {
            // (op, left or NULL for the prefix `~`, right)
            need(3)?;
            let left = if s(0) == "~" { None } else { Some(arg(1)) };
            crate::net::arith(&s(0), left.as_ref(), &arg(2))
        }
        "__coll_key" => {
            need(2)?;
            crate::collation::sort_key(&s(0), &s(1)).map(Bson::String)
        }
        "__coll_keyv" => {
            need(2)?;
            crate::collation::sort_key_with_value(&s(0), &s(1)).map(Bson::String)
        }
        "__coll_value" => {
            need(1)?;
            Ok(Bson::String(crate::collation::key_value(&s(0))))
        }
        "__net_diff" => {
            need(2)?;
            crate::net::diff(&arg(0), &arg(1))
        }
        "__net_sortkey" => {
            need(1)?;
            Ok(match arg(0) {
                Bson::Null => Bson::Null,
                v => crate::net::sort_key(&text(&v)).map_or(Bson::Null, Bson::String),
            })
        }
        "statement_timestamp" => {
            need(0)?;
            Ok(statement_value())
        }
        "clock_timestamp" => {
            need(0)?;
            Ok(clock_value())
        }
        "similar_to_escape" | "similar_escape" => {
            let esc = if args.len() > 1 { Some(s(1)) } else { None };
            similar_escape(&s(0), esc.as_deref()).map(Bson::String)
        }
        "to_char" => {
            need(2)?;
            match crate::formatting::to_char_value(&arg(0), &s(1), false) {
                Some(out) => out.map(Bson::String),
                None => Err(Error::Unsupported("to_char() of this type".into())),
            }
        }
        "to_number" => {
            need(2)?;
            match crate::formatting::to_number(&s(0), &s(1))? {
                Some(text) => crate::cast_value(Bson::String(text), "numeric"),
                None => Ok(Bson::Null),
            }
        }
        // A version-4 UUID: 122 random bits, the version nibble 4 and the
        // RFC 4122 variant bits.
        "gen_random_uuid" | "uuid_generate_v4" => {
            need(0)?;
            let (hi, lo) = (random_u64(), random_u64());
            let hi = (hi & !0xF000) | 0x4000;
            let lo = (lo & !(0b11 << 62)) | (0b10 << 62);
            let text = format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                hi >> 32,
                (hi >> 16) & 0xFFFF,
                hi & 0xFFFF,
                lo >> 48,
                lo & 0xFFFF_FFFF_FFFF
            );
            crate::cast_value(Bson::String(text), "uuid")
        }
        // Uniform in [0, 1), from 53 random bits -- a double's mantissa.
        "random" => {
            need(0)?;
            Ok(Bson::Double(
                (random_u64() >> 11) as f64 / (1u64 << 53) as f64,
            ))
        }
        "st_geomfromgeojson" => {
            need(1)?;
            let g = crate::geometry::from_geojson(&s(0))?;
            Ok(Bson::String(crate::geometry::to_hex(&g)))
        }
        "st_geomfromtext" | "st_geomfromewkt" => {
            let mut g = crate::geometry::from_wkt(&s(0))?;
            if let Some(srid) = args.get(1) {
                let srid = match srid {
                    Bson::Int32(n) => i64::from(*n),
                    Bson::Int64(n) => *n,
                    Bson::Double(n) => *n as i64,
                    _ => 0,
                };
                g.srid = i32::try_from(srid).unwrap_or(0);
            }
            Ok(Bson::String(crate::geometry::to_hex(&g)))
        }
        "st_srid" => {
            need(1)?;
            let g = crate::geometry::from_text(&s(0))?;
            Ok(Bson::Int32(g.srid))
        }
        "st_astext" | "st_asewkt" => {
            need(1)?;
            let g = crate::geometry::from_text(&s(0))?;
            Ok(Bson::String(crate::geometry::to_wkt(
                &g,
                name == "st_asewkt",
            )))
        }
        "hstore" => {
            need(2)?;
            let pairs = match (&args[0], &args[1]) {
                (Bson::Array(keys), Bson::Array(vals)) => {
                    if keys.len() != vals.len() {
                        return Err(Error::DataException("arrays must have same bounds".into()));
                    }
                    keys.iter()
                        .zip(vals)
                        .map(|(k, v)| {
                            (
                                text(k),
                                if *v == Bson::Null {
                                    None
                                } else {
                                    Some(text(v))
                                },
                            )
                        })
                        .collect()
                }
                (Bson::Array(_), _) | (_, Bson::Array(_)) => return Err(wrong_args(name)),
                _ => vec![(s(0), Some(s(1)))],
            };
            Ok(Bson::String(crate::hstore::render(&crate::hstore::unique(
                pairs,
            ))))
        }
        "akeys" | "skeys" => {
            need(1)?;
            let pairs = crate::hstore::parse(&s(0))?;
            Ok(Bson::Array(
                pairs.into_iter().map(|(k, _)| Bson::String(k)).collect(),
            ))
        }
        "avals" | "svals" => {
            need(1)?;
            let pairs = crate::hstore::parse(&s(0))?;
            Ok(Bson::Array(
                pairs
                    .into_iter()
                    .map(|(_, v)| v.map(Bson::String).unwrap_or(Bson::Null))
                    .collect(),
            ))
        }
        "exist" => {
            need(2)?;
            let pairs = crate::hstore::parse(&s(0))?;
            Ok(Bson::Boolean(crate::hstore::has_key(&pairs, &s(1))))
        }
        "defined" => {
            need(2)?;
            let pairs = crate::hstore::parse(&s(0))?;
            Ok(Bson::Boolean(crate::hstore::get(&pairs, &s(1)).is_some()))
        }
        "delete" => {
            need(2)?;
            let mut pairs = crate::hstore::parse(&s(0))?;
            let key = s(1);
            pairs.retain(|(k, _)| *k != key);
            Ok(Bson::String(crate::hstore::render(&pairs)))
        }
        "hstore_to_json" | "hstore_to_jsonb" => {
            need(1)?;
            let pairs = crate::hstore::parse(&s(0))?;
            let obj = crate::json::Json::Object(
                pairs
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            v.map(crate::json::Json::Str)
                                .unwrap_or(crate::json::Json::Null),
                        )
                    })
                    .collect(),
            );
            Ok(Bson::String(if name == "hstore_to_json" {
                crate::json::render_json(&obj)
            } else {
                crate::json::render_jsonb(&obj)
            }))
        }
        // Unicode-aware, as a UTF-8-locale PostgreSQL maps case -- but the
        // SIMPLE mapping (towupper / towlower, one character to one), never
        // the full one: `upper('ß')` is `ß`, not `SS`. `İ` lowers to `i`, as
        // glibc has it. (This box's reference runs lc_ctype=C, which maps no
        // non-ASCII letter at all; tasks/backlog.md records why that is not
        // matched.)
        "upper" => {
            need(1)?;
            Ok(Bson::String(s(0).chars().map(simple_upper).collect()))
        }
        "lower" => {
            need(1)?;
            Ok(Bson::String(s(0).chars().map(simple_lower).collect()))
        }
        "initcap" => {
            need(1)?;
            let mut out = String::new();
            let mut fresh = true;
            for c in s(0).chars() {
                if c.is_alphanumeric() {
                    if fresh {
                        out.push(simple_upper(c));
                    } else {
                        out.push(simple_lower(c));
                    }
                    fresh = false;
                } else {
                    out.push(c);
                    fresh = true;
                }
            }
            Ok(Bson::String(out))
        }
        // `length` counts CHARACTERS; `octet_length` counts bytes. They differ
        // the moment the text is not ASCII.
        "length" | "char_length" | "character_length" => {
            need(1)?;
            // `length(bytea)` is a BYTE count, not a character count.
            if let Bson::Binary(b) = &arg(0) {
                return Ok(Bson::Int32(b.bytes.len() as i32));
            }
            Ok(Bson::Int32(s(0).chars().count() as i32))
        }
        "octet_length" => {
            need(1)?;
            if let Bson::Binary(b) = &arg(0) {
                return Ok(Bson::Int32(b.bytes.len() as i32));
            }
            Ok(Bson::Int32(s(0).len() as i32))
        }
        "bit_length" => {
            need(1)?;
            if let Bson::Binary(b) = &arg(0) {
                return Ok(Bson::Int32((b.bytes.len() * 8) as i32));
            }
            Ok(Bson::Int32((s(0).len() * 8) as i32))
        }
        "get_byte" => {
            need(2)?;
            let bytes = crate::bytea::parse(&arg(0))?;
            let n = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            crate::bytea::get_byte(&bytes, n).map(Bson::Int32)
        }
        "set_byte" => {
            need(3)?;
            let bytes = crate::bytea::parse(&arg(0))?;
            let n = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            let v = as_i64(&arg(2)).ok_or_else(|| wrong_args(name))?;
            crate::bytea::set_byte(&bytes, n, v).map(crate::bytea::to_binary)
        }
        "encode" => {
            need(2)?;
            let bytes = crate::bytea::parse(&arg(0))?;
            crate::bytea::encode(&bytes, &s(1)).map(Bson::String)
        }
        "decode" => {
            need(2)?;
            crate::bytea::decode(&s(0), &s(1)).map(crate::bytea::to_binary)
        }
        "btrim" | "trim" | "ltrim" | "rtrim" => {
            if args.is_empty() || args.len() > 2 {
                return Err(wrong_args(name));
            }
            let subject = s(0);
            let set: Vec<char> = if args.len() == 2 {
                s(1).chars().collect()
            } else {
                vec![' ']
            };
            let trimmed = match name {
                "ltrim" => subject.trim_start_matches(|c| set.contains(&c)).to_string(),
                "rtrim" => subject.trim_end_matches(|c| set.contains(&c)).to_string(),
                _ => subject.trim_matches(|c| set.contains(&c)).to_string(),
            };
            Ok(Bson::String(trimmed))
        }
        "substr" | "substring" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(wrong_args(name));
            }
            // `substring(s FROM pattern)` is a different function sharing the
            // name: a STRING second argument is a POSIX pattern, not an offset,
            // and answers the first capture group when the pattern has one and
            // the whole match otherwise. With a third string argument it is
            // the SQL-standard form, whose `%` / `_` and `#"..."#` delimiters
            // mark the part to return.
            //
            // Told apart by the ARGUMENT's type, as PostgreSQL's own overload
            // resolution does. Reading the second argument as an integer
            // regardless answered `42601 function substring does not exist
            // with that argument list` for every regex use.
            if matches!(arg(1), Bson::String(_)) {
                return substring_pattern(&s(0), &s(1), args.get(2).map(text));
            }
            let start = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            let len = if args.len() == 3 {
                let l = as_i64(&arg(2)).ok_or_else(|| wrong_args(name))?;
                if l < 0 {
                    return Err(Error::SubstringError(
                        "negative substring length not allowed".into(),
                    ));
                }
                Some(l)
            } else {
                None
            };
            Ok(Bson::String(substring(&s(0), start, len)))
        }
        "replace" => {
            need(3)?;
            Ok(Bson::String(s(0).replace(&s(1), &s(2))))
        }
        "repeat" => {
            need(2)?;
            let n = as_i64(&arg(1)).unwrap_or(0).max(0) as usize;
            Ok(Bson::String(s(0).repeat(n)))
        }
        "reverse" => {
            need(1)?;
            Ok(Bson::String(s(0).chars().rev().collect()))
        }
        "left" | "right" => {
            need(2)?;
            let chars: Vec<char> = s(0).chars().collect();
            let n = as_i64(&arg(1)).unwrap_or(0);
            // A negative count means "all but this many from the other end".
            let take = if n >= 0 {
                (n as usize).min(chars.len())
            } else {
                chars.len().saturating_sub(n.unsigned_abs() as usize)
            };
            let out: String = if name == "left" {
                chars.iter().take(take).collect()
            } else {
                chars.iter().skip(chars.len() - take).collect()
            };
            Ok(Bson::String(out))
        }
        // 1-based, and 0 when absent.
        "strpos" | "position" => {
            need(2)?;
            let (haystack, needle) = (s(0), s(1));
            Ok(Bson::Int32(match haystack.find(&needle) {
                Some(byte_idx) => haystack[..byte_idx].chars().count() as i32 + 1,
                None => 0,
            }))
        }
        "concat" => Ok(Bson::String(
            args.iter()
                .filter(|a| *a != &Bson::Null)
                .map(text)
                .collect::<Vec<_>>()
                .join(""),
        )),
        "concat_ws" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            let sep = s(0);
            Ok(Bson::String(
                args[1..]
                    .iter()
                    .filter(|a| *a != &Bson::Null)
                    .map(text)
                    .collect::<Vec<_>>()
                    .join(&sep),
            ))
        }
        "sha224" | "sha256" | "sha384" | "sha512" => {
            use sha2::Digest;
            let bytes = match &args[0] {
                Bson::Binary(b) => b.bytes.clone(),
                other => text(other).into_bytes(),
            };
            let digest: Vec<u8> = match name {
                "sha224" => sha2::Sha224::digest(&bytes).to_vec(),
                "sha256" => sha2::Sha256::digest(&bytes).to_vec(),
                "sha384" => sha2::Sha384::digest(&bytes).to_vec(),
                _ => sha2::Sha512::digest(&bytes).to_vec(),
            };
            Ok(Bson::Binary(bson::Binary {
                subtype: bson::spec::BinarySubtype::Generic,
                bytes: digest,
            }))
        }
        "md5" => {
            need(1)?;
            Ok(Bson::String(md5_hex(s(0).as_bytes())))
        }
        "quote_ident" => {
            need(1)?;
            Ok(Bson::String(quote_identifier(&s(0))))
        }
        "format" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            // A NULL format string is a NULL answer; NULL ARGUMENTS are
            // formatted (`%s` as empty, `%L` as the bare word NULL).
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            pg_format(&s(0), &args[1..]).map(Bson::String)
        }
        "chr" => {
            need(1)?;
            let n = as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let c = u32::try_from(n)
                .ok()
                .filter(|n| *n != 0)
                .and_then(char::from_u32)
                .ok_or_else(|| {
                    Error::InvalidText(format!("requested character too large for encoding: {n}"))
                })?;
            Ok(Bson::String(c.to_string()))
        }
        "ascii" => {
            need(1)?;
            Ok(Bson::Int32(s(0).chars().next().map_or(0, |c| c as i32)))
        }
        // `split_part(s, sep, n)` -- a NEGATIVE `n` counts from the END
        // (`split_part('a,b,c', ',', -1)` is `c`), and only ZERO is an error.
        // Refusing every non-positive field rejected a form PostgreSQL has
        // supported since 14, and the message named the wrong rule: it says
        // "must not be zero", not "must be greater than zero".
        //
        // An EMPTY separator is not a split at all: the whole string is field
        // one. Rust's `split("")` yields empty edge pieces instead.
        "split_part" => {
            need(3)?;
            let n = as_i64(&arg(2)).unwrap_or(0);
            if n == 0 {
                return Err(Error::InvalidParameter(
                    "field position must not be zero".into(),
                ));
            }
            let subject = s(0);
            let sep = s(1);
            let parts: Vec<&str> = if sep.is_empty() {
                vec![subject.as_str()]
            } else {
                subject.split(&sep as &str).collect()
            };
            let idx = if n > 0 {
                (n - 1) as usize
            } else {
                match parts.len().checked_sub((-n) as usize) {
                    Some(i) => i,
                    None => return Ok(Bson::String(String::new())),
                }
            };
            Ok(Bson::String(
                parts.get(idx).copied().unwrap_or("").to_string(),
            ))
        }
        "starts_with" => {
            need(2)?;
            Ok(Bson::Boolean(s(0).starts_with(&s(1))))
        }
        // `lpad` / `rpad` count CHARACTERS, not bytes, and TRUNCATE when the
        // target is shorter than the input: `lpad('abcdef', 3)` is `abc`. A
        // target of 0 or less is the empty string, and an EMPTY fill cannot
        // pad, so the input comes back unchanged.
        "lpad" | "rpad" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(wrong_args(name));
            }
            let subject: Vec<char> = s(0).chars().collect();
            let width = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            if width <= 0 {
                return Ok(Bson::String(String::new()));
            }
            let width = width as usize;
            if subject.len() >= width {
                return Ok(Bson::String(subject[..width].iter().collect()));
            }
            let fill: Vec<char> = if args.len() == 3 {
                s(2).chars().collect()
            } else {
                vec![' ']
            };
            if fill.is_empty() {
                return Ok(Bson::String(subject.iter().collect()));
            }
            let pad: String = fill.iter().cycle().take(width - subject.len()).collect();
            let body: String = subject.iter().collect();
            Ok(Bson::String(if name == "lpad" {
                format!("{pad}{body}")
            } else {
                format!("{body}{pad}")
            }))
        }
        // `to_hex`'s WIDTH follows the argument's TYPE, not its value: an
        // `int4` -1 is `ffffffff` and an `int8` -1 is `ffffffffffffffff`.
        // Formatting from the value alone would collapse the two.
        "to_hex" => {
            need(1)?;
            Ok(Bson::String(match arg(0) {
                Bson::Int32(i) => format!("{:x}", i as u32),
                Bson::Int64(i) => format!("{:x}", i as u64),
                other => match as_i64(&other) {
                    Some(i) => format!("{:x}", i as u64),
                    None => return Err(wrong_args(name)),
                },
            }))
        }
        // `translate(s, from, to)` -- a character at position i in `from`
        // becomes `to[i]`, or is DELETED when `to` is shorter. Only the FIRST
        // occurrence of a character in `from` counts.
        "translate" => {
            need(3)?;
            let from: Vec<char> = s(1).chars().collect();
            let to: Vec<char> = s(2).chars().collect();
            let mut out = String::new();
            for c in s(0).chars() {
                match from.iter().position(|f| *f == c) {
                    Some(i) => {
                        if let Some(r) = to.get(i) {
                            out.push(*r);
                        }
                    }
                    None => out.push(c),
                }
            }
            Ok(Bson::String(out))
        }
        // `overlay(s placing r from p [for n])` -- replace `n` characters at
        // 1-based `p` with `r`; `n` defaults to `r`'s own length. A `p` below
        // 1 is PostgreSQL's own "negative substring length not allowed", the
        // same error the `substring` it is defined in terms of gives.
        "overlay" => {
            if args.len() < 3 || args.len() > 4 {
                return Err(wrong_args(name));
            }
            let subject: Vec<char> = s(0).chars().collect();
            let placing: Vec<char> = s(1).chars().collect();
            let from = as_i64(&arg(2)).ok_or_else(|| wrong_args(name))?;
            if from < 1 {
                return Err(Error::SubstringError(
                    "negative substring length not allowed".into(),
                ));
            }
            let count = match args.len() {
                4 => as_i64(&arg(3)).ok_or_else(|| wrong_args(name))?,
                _ => placing.len() as i64,
            };
            if count < 0 {
                return Err(Error::SubstringError(
                    "negative substring length not allowed".into(),
                ));
            }
            let head_end = ((from - 1) as usize).min(subject.len());
            let tail_start = (((from - 1) + count) as usize).min(subject.len());
            let mut out: String = subject[..head_end].iter().collect();
            out.extend(placing.iter());
            out.extend(subject[tail_start..].iter());
            Ok(Bson::String(out))
        }
        // `quote_literal` is NULL-propagating; `quote_nullable` answers the
        // four-character string `NULL` instead -- which is the whole reason
        // the two exist as a pair, and which `psql` renders identically to a
        // real NULL, so probe it with `IS NULL` rather than by eye.
        "quote_literal" | "quote_nullable" => {
            need(1)?;
            if arg(0) == Bson::Null {
                return Ok(if name == "quote_nullable" {
                    Bson::String("NULL".into())
                } else {
                    Bson::Null
                });
            }
            Ok(Bson::String(quote_literal_text(&s(0))))
        }
        "regexp_split_to_array" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(wrong_args(name));
            }
            let flags = if args.len() == 3 { s(2) } else { String::new() };
            let re = regex::RegexBuilder::new(&pg_regex_source(&s(1)))
                .case_insensitive(flags.contains('i'))
                .build()
                .map_err(|e| Error::InvalidText(format!("invalid regular expression: {e}")))?;
            let subject = s(0);
            // An EMPTY pattern splits into characters, as PostgreSQL does;
            // Rust's own split on an empty match yields empty edge pieces.
            let parts: Vec<Bson> = if re.as_str().is_empty() {
                subject
                    .chars()
                    .map(|c| Bson::String(c.to_string()))
                    .collect()
            } else {
                re.split(&subject)
                    .map(|p| Bson::String(p.to_string()))
                    .collect()
            };
            Ok(Bson::Array(parts))
        }
        "unistr" => {
            need(1)?;
            Ok(Bson::String(unistr(&s(0))?))
        }
        // `normalize(text [, form])` -- Unicode normalisation, NFC by default.
        // The form arrives as an ordinary string constant (`NFD` and friends
        // are grammar keywords, so an unknown one is a SYNTAX error before it
        // ever reaches here, and needs no check).
        "normalize" => {
            if args.is_empty() || args.len() > 2 {
                return Err(wrong_args(name));
            }
            use unicode_normalization::UnicodeNormalization;
            let subject = s(0);
            let form = if args.len() == 2 {
                s(1).to_ascii_uppercase()
            } else {
                "NFC".to_string()
            };
            Ok(Bson::String(match form.as_str() {
                "NFC" => subject.nfc().collect(),
                "NFD" => subject.nfd().collect(),
                "NFKC" => subject.nfkc().collect(),
                "NFKD" => subject.nfkd().collect(),
                other => {
                    return Err(Error::InvalidParameter(format!(
                        "invalid normalization form: {other}"
                    )))
                }
            }))
        }
        "regexp_count" | "regexp_instr" | "regexp_substr" | "regexp_like" => {
            regexp_function(name, args)
        }
        // `s IS [form] NORMALIZED`: whether `normalize(s, form)` is `s`.
        "is_normalized" => {
            if args.is_empty() || args.len() > 2 {
                return Err(wrong_args(name));
            }
            let normal = eval("normalize", args)?;
            Ok(Bson::Boolean(normal == Bson::String(s(0))))
        }
        // `convert_from(bytea, encoding)` -- decode stored bytes as text.
        "convert_from" => {
            need(2)?;
            let bytes = match arg(0) {
                Bson::Binary(b) => b.bytes,
                other => text(&other).into_bytes(),
            };
            let encoding = s(1).to_ascii_uppercase().replace(['-', '_'], "");
            match encoding.as_str() {
                "UTF8" | "UNICODE" => String::from_utf8(bytes).map(Bson::String).map_err(|_| {
                    Error::InvalidText("invalid byte sequence for encoding \"UTF8\"".into())
                }),
                // The LATIN family is single-byte: each octet IS its codepoint.
                e if e.starts_with("LATIN") || e == "SQLASCII" => {
                    Ok(Bson::String(bytes.iter().map(|b| *b as char).collect()))
                }
                other => Err(Error::InvalidText(format!(
                    "invalid encoding name \"{other}\""
                ))),
            }
        }
        // --- numeric -------------------------------------------------------
        // `abs` gives back the type it was handed, so an exact numeric stays
        // exact rather than becoming a float.
        "abs" => {
            need(1)?;
            Ok(match arg(0) {
                Bson::Int32(i) => Bson::Int32(
                    i.checked_abs()
                        .ok_or_else(|| Error::NumericOutOfRange("integer out of range".into()))?,
                ),
                Bson::Int64(i) => Bson::Int64(
                    i.checked_abs()
                        .ok_or_else(|| Error::NumericOutOfRange("bigint out of range".into()))?,
                ),
                Bson::Double(d) => Bson::Double(d.abs()),
                v if crate::is_numeric(&v) => {
                    let t = crate::numeric_text(&v).unwrap_or_default();
                    parse_numeric(t.strip_prefix('-').unwrap_or(&t))?
                }
                other => return Err(Error::Unsupported(format!("abs of {other:?}"))),
            })
        }
        "sign" => {
            need(1)?;
            let f = as_f64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let out = if f > 0.0 {
                1
            } else if f < 0.0 {
                -1
            } else {
                0
            };
            // `sign` answers `float8` for a float or an integer, and `numeric`
            // only when handed one -- so `sign(-3)` is `-1.0`, not `-1`.
            Ok(match arg(0) {
                v if crate::is_numeric(&v) => {
                    parse_numeric(&out.to_string()).expect("a one-digit decimal")
                }
                _ => Bson::Double(f64::from(out)),
            })
        }
        "ceil" | "ceiling" | "floor" | "trunc" | "round" => numeric_rounding(name, args),
        // Over a `numeric` argument these are numeric functions, with
        // numeric.c's result scales (`numeric_math`); two-argument `log`
        // exists ONLY over numeric. Otherwise they are the float8 ones.
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow"
            if (args.iter().any(crate::is_numeric) || (name == "log" && args.len() == 2))
                && !args.iter().any(|a| matches!(a, Bson::Double(_))) =>
        {
            if args.contains(&Bson::Null) {
                return Ok(Bson::Null);
            }
            let texts: Option<Vec<String>> = args
                .iter()
                .map(crate::numeric::numeric_operand_text)
                .collect();
            let texts = texts.ok_or_else(|| wrong_args(name))?;
            crate::numeric_math::call(name, &texts).unwrap_or_else(|| Err(wrong_args(name)))
        }
        "numeric_send" => {
            need(1)?;
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            let text =
                crate::numeric::numeric_operand_text(&arg(0)).ok_or_else(|| wrong_args(name))?;
            Ok(crate::bytea::to_binary(crate::numeric_math::numeric_send(
                &text,
            )))
        }
        "scale" | "min_scale" | "trim_scale" => {
            need(1)?;
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            let text =
                crate::numeric::numeric_operand_text(&arg(0)).ok_or_else(|| wrong_args(name))?;
            crate::numeric_math::call(name, &[text]).unwrap_or_else(|| Err(wrong_args(name)))
        }
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow" => float_math(name, args),
        "mod" => {
            need(2)?;
            match (arg(0), arg(1)) {
                (_, b) if as_f64(&b) == Some(0.0) => Err(Error::DivisionByZero),
                (Bson::Int32(a), b) => Ok(Bson::Int32(
                    a % i32::try_from(as_i64(&b).unwrap_or(1)).unwrap_or(1),
                )),
                (a, b) => {
                    let (x, y) = (
                        as_i64(&a).ok_or_else(|| wrong_args(name))?,
                        as_i64(&b).ok_or_else(|| wrong_args(name))?,
                    );
                    Ok(Bson::Int64(x % y))
                }
            }
        }
        // `div` is defined on `numeric`, so integer arguments are coerced and
        // the answer is a `numeric` -- not the `int8` the arithmetic suggests.
        "div" => {
            need(2)?;
            let (a, b) = (
                as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?,
                as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?,
            );
            if b == 0 {
                return Err(Error::DivisionByZero);
            }
            parse_numeric(&(a / b).to_string())
        }
        "greatest" | "least" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            // NULLs are IGNORED here, so an all-NULL call answers NULL and a
            // mixed one answers the extreme of the non-NULLs.
            let mut best: Option<Bson> = None;
            for a in args.iter().filter(|a| *a != &Bson::Null) {
                best = Some(match best {
                    None => a.clone(),
                    Some(cur) => {
                        let take = match compare_constants(a, &cur) {
                            Some(o) => {
                                (name == "greatest") == (o == std::cmp::Ordering::Greater)
                                    && o != std::cmp::Ordering::Equal
                            }
                            None => false,
                        };
                        if take {
                            a.clone()
                        } else {
                            cur
                        }
                    }
                });
            }
            // The result has the arguments' COMMON type: `least(1, 2.5)` is
            // the numeric 1, `greatest(1, 2.5::float8)` a float8.
            let any_double = args.iter().any(|a| matches!(a, Bson::Double(_)));
            let any_numeric = args.iter().any(crate::is_numeric);
            let any_int64 = args.iter().any(|a| matches!(a, Bson::Int64(_)));
            Ok(match best {
                Some(Bson::Int32(i)) if any_double => Bson::Double(f64::from(i)),
                Some(Bson::Int64(i)) if any_double => Bson::Double(i as f64),
                Some(v) if any_double && crate::is_numeric(&v) => Bson::Double(
                    crate::numeric_text(&v)
                        .and_then(|t| crate::numeric::numeric_text_to_f64(&t))
                        .unwrap_or(f64::NAN),
                ),
                Some(Bson::Int32(i)) if any_numeric => crate::numeric::numeric_bson(&i.to_string()),
                Some(Bson::Int64(i)) if any_numeric => crate::numeric::numeric_bson(&i.to_string()),
                Some(Bson::Int32(i)) if any_int64 => Bson::Int64(i64::from(i)),
                Some(v) => v,
                None => Bson::Null,
            })
        }
        // The catalog-facing functions, which a client calls INSIDE an
        // expression at least as often as on its own: `version() LIKE
        // 'PostgreSQL%'`, `current_setting('x') ~ '...'`,
        // `obj_description(oid) IS NULL`. As a bare select-list target they
        // become a `ConstCol` the server resolves; reaching them here is what
        // makes the expression form work too.
        "version" | "current_schema" => crate::session_function(name)
            .ok_or_else(|| Error::Unsupported(format!("function {name}()"))),
        "current_database" | "current_catalog" => Ok(Bson::String(crate::session_database())),
        "current_setting" => {
            let Some(Bson::String(key)) = args.first() else {
                return Ok(Bson::Null);
            };
            let missing_ok = matches!(args.get(1), Some(Bson::Boolean(true)));
            match crate::session_setting(key) {
                Some(v) => Ok(Bson::String(v)),
                // PostgreSQL's own refusal for an unknown GUC, unless the
                // second argument asked for NULL instead.
                None if missing_ok => Ok(Bson::Null),
                None => Err(Error::UndefinedObject(format!(
                    "unrecognized configuration parameter \"{key}\""
                ))),
            }
        }
        // `COMMENT ON` text, published by the server per catalog version:
        // `obj_description(oid [, catalog])` is an object's own comment,
        // `col_description(table_oid, attnum)` a column's. NULL for an
        // uncommented object, as in PostgreSQL.
        "obj_description" | "col_description" | "shobj_description" => {
            let oid = match args.first() {
                Some(Bson::Int32(i)) => i64::from(*i),
                Some(Bson::Int64(i)) => *i,
                Some(other) => match crate::regclass_oid(other) {
                    Some(o) => o,
                    None => return Ok(Bson::Null),
                },
                None => return Ok(Bson::Null),
            };
            let subid = if name == "col_description" {
                match args.get(1) {
                    Some(Bson::Int32(i)) => *i,
                    Some(Bson::Int64(i)) => i32::try_from(*i).unwrap_or(-1),
                    _ => return Ok(Bson::Null),
                }
            } else {
                0
            };
            Ok(crate::object_comment(oid, subid).map_or(Bson::Null, Bson::String))
        }
        // `pg_get_expr(adbin, adrelid)` renders a stored expression. This
        // server keeps `adbin` as the rendered TEXT already (see the
        // `pg_attrdef` rows), so it hands the first argument straight back.
        "pg_get_expr" => Ok(args.first().cloned().unwrap_or(Bson::Null)),
        // Every object lives on the one search path (`public`, then
        // `pg_catalog`), so any object is visible by its bare name.
        // Every table is a regular, permanent one or a view; a view (or a
        // missing oid) is not publishable, which only the catalog can tell,
        // and psql's publication footer asks it about tables.
        // A function body is kept as its source text (`prosrc`), never as
        // a SQL-standard `BEGIN ATOMIC` parse tree, so there is none.
        "pg_get_function_sqlbody" => Ok(Bson::Null),
        "pg_get_userbyid" => Ok(match args.first() {
            None | Some(Bson::Null) => Bson::Null,
            Some(Bson::Int32(i)) => Bson::String(crate::regobj::role_name(i64::from(*i))),
            Some(Bson::Int64(i)) => Bson::String(crate::regobj::role_name(*i)),
            Some(other) => Bson::String(crate::regobj::role_name(
                crate::value_text(other).trim().parse().unwrap_or(-1),
            )),
        }),
        "pg_relation_is_publishable" => Ok(match args.first() {
            None | Some(Bson::Null) => Bson::Null,
            Some(_) => Bson::Boolean(true),
        }),
        n if n.starts_with("pg_") && n.ends_with("_is_visible") => Ok(match args.first() {
            None | Some(Bson::Null) => Bson::Null,
            Some(_) => Bson::Boolean(true),
        }),
        "pg_size_pretty" => size_pretty(&args[0]),
        "pg_size_bytes" => size_bytes(&crate::value_text(&args[0])),
        "pg_column_size" => Ok(column_size(&args[0])),
        // A PROCEDURE is not callable in an expression.
        _ if crate::correlated::is_user_procedure(name, args.len()) => Err(Error::Sqlstate(
            "42809",
            format!(
                "{name}({}) is a procedure",
                args.iter()
                    .map(|a| match a {
                        Bson::String(_) | Bson::Null => "unknown".to_string(),
                        other => crate::display_type(crate::inferred_type(other)),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
        // PostgreSQL names the argument types it could not match; a
        // constant string argument is an untyped literal, `unknown`.
        _ => Err(Error::Unsupported(format!(
            "function {name}({})",
            args.iter()
                .map(|a| match a {
                    Bson::String(_) | Bson::Null => "unknown".to_string(),
                    other => crate::display_type(crate::inferred_type(other)),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// `format_type(oid, typmod)` renders a type the way PostgreSQL writes it in
/// an error or a `\d`: `integer`, `numeric(10,2)`.
fn format_type_call(args: &[Bson]) -> Result<Bson> {
    let Some(oid) = args.first().and_then(|v| match v {
        Bson::Int32(x) => Some(i64::from(*x)),
        Bson::Int64(x) => Some(*x),
        _ => None,
    }) else {
        // A NULL OID is the one argument that DOES make it NULL.
        return Ok(Bson::Null);
    };
    let Some(name) = crate::pgtypes::name_of_oid(oid) else {
        // A user type (enum, composite, range, domain, ...) prints as its
        // regtype name; only an oid nothing knows is `???`.
        let text = crate::regtype_text(oid);
        return Ok(Bson::String(if text == oid.to_string() {
            format!("???({oid})")
        } else {
            text
        }));
    };
    let typmod = args.get(1).and_then(|v| match v {
        Bson::Int32(x) => Some(*x),
        Bson::Int64(x) => i32::try_from(*x).ok(),
        _ => None,
    });
    Ok(Bson::String(format_type_text(name, typmod)))
}

/// `format_type`'s rendering: the display name, with the declared width or
/// precision put back on when the modifier carries one.
pub(crate) fn format_type_text_public(name: &str, typmod: Option<i32>) -> String {
    format_type_text(name, typmod)
}

pub(crate) fn format_type_text(name: &str, typmod: Option<i32>) -> String {
    let display = crate::display_type(name);
    let Some(typmod) = typmod.filter(|m| *m >= 4) else {
        return display;
    };
    match name {
        // A `numeric`'s two numbers are packed into one modifier.
        "numeric" | "decimal" => {
            let packed = typmod - 4;
            format!("{display}({},{})", packed >> 16, packed & 0xffff)
        }
        "varchar" | "bpchar" => format!("{display}({})", typmod - 4),
        _ => display,
    }
}

/// `ceil` / `floor` / `trunc` / `round`, which keep an exact input exact.
///
/// `round(numeric)` goes half AWAY FROM ZERO and `round(float8)` goes half TO
/// EVEN — the same split the integer casts have, and the same trap.
fn numeric_rounding(name: &str, args: &[Bson]) -> Result<Bson> {
    let subject = args.first().cloned().unwrap_or(Bson::Null);
    // `round(x, n)` is numeric-only in PostgreSQL and keeps n decimal places.
    if args.len() == 2 {
        if name != "round" && name != "trunc" {
            return Err(wrong_args(name));
        }
        let places = as_i64(&args[1]).unwrap_or(0);
        let text = text(&subject);
        return round_decimal_text(&text, places, name == "round").ok_or_else(|| wrong_args(name));
    }
    if args.len() != 1 {
        return Err(wrong_args(name));
    }
    Ok(match subject {
        // An integer has no rounding overload of its own: PostgreSQL picks
        // the float8 one (float8 is its category's preferred type), so
        // `round(1)` is the double `1`.
        Bson::Int32(i) => Bson::Double(f64::from(i)),
        Bson::Int64(i) => Bson::Double(i as f64),
        Bson::Double(d) => Bson::Double(match name {
            "ceil" | "ceiling" => d.ceil(),
            "floor" => d.floor(),
            "trunc" => d.trunc(),
            _ => d.round_ties_even(),
        }),
        ref v if crate::is_numeric(v) => {
            let t = text(&subject);
            let places = 0;
            let out = match name {
                "ceil" | "ceiling" => decimal_ceil_floor(&t, true),
                "floor" => decimal_ceil_floor(&t, false),
                "trunc" => round_decimal_text(&t, places, false),
                _ => round_decimal_text(&t, places, true),
            };
            out.ok_or_else(|| wrong_args(name))?
        }
        other => return Err(Error::Unsupported(format!("{name} of {other:?}"))),
    })
}

/// Round or truncate a decimal to `places`, on the DIGITS. Rounding is half
/// away from zero, which is what PostgreSQL does for `numeric`.
fn round_decimal_text(text: &str, places: i64, round: bool) -> Option<Bson> {
    let (neg, body) = match text.trim().strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, text.trim()),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let places = places.max(0) as usize;
    let mut digits: Vec<u8> = format!("{int_part}{frac_part}").into_bytes();
    let frac_len = frac_part.len();
    if places >= frac_len {
        // Nothing to remove; pad so the scale is exactly `places`.
        let mut out = String::new();
        if neg {
            out.push('-');
        }
        out.push_str(int_part);
        if places > 0 {
            out.push('.');
            out.push_str(&format!("{frac_part:0<places$}"));
        }
        return parse_numeric(&out).ok();
    }
    let drop = frac_len - places;
    let keep = digits.len() - drop;
    let round_up = round && digits.get(keep).is_some_and(|d| *d >= b'5');
    digits.truncate(keep);
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s: String = String::from_utf8(digits).ok()?;
    let split = s.len().saturating_sub(places);
    let (whole, frac) = s.split_at(split);
    let whole = if whole.is_empty() { "0" } else { whole };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(whole);
    if places > 0 {
        out.push('.');
        out.push_str(frac);
    }
    parse_numeric(&out).ok()
}

fn decimal_ceil_floor(text: &str, up: bool) -> Option<Bson> {
    let truncated = round_decimal_text(text, 0, false)?;
    let neg = text.trim_start().starts_with('-');
    let has_fraction = text
        .split_once('.')
        .is_some_and(|(_, f)| f.chars().any(|c| c != '0'));
    if !has_fraction {
        return Some(truncated);
    }
    let adjust = if up { !neg } else { neg };
    if !adjust {
        return Some(truncated);
    }
    let one = if neg { "-1" } else { "1" };
    match decimal_arith("+", &crate::numeric_text(&truncated)?, one) {
        Some(Ok(v)) => Some(v),
        _ => None,
    }
}

/// The float-valued family. PostgreSQL answers `float8` for all of these when
/// given a float or an integer, and `numeric` only where the input is one --
/// which for `ln` / `log` / `sqrt` it keeps, so those are left to the float
/// path here and their exactness is not claimed.
fn float_math(name: &str, args: &[Bson]) -> Result<Bson> {
    let f =
        |i: usize| -> Result<f64> { args.get(i).and_then(as_f64).ok_or_else(|| wrong_args(name)) };
    let out = match name {
        "sqrt" => {
            let x = f(0)?;
            if x < 0.0 {
                return Err(Error::InvalidText(
                    "cannot take square root of a negative number".into(),
                ));
            }
            x.sqrt()
        }
        "exp" => f(0)?.exp(),
        "ln" => {
            let x = f(0)?;
            if x <= 0.0 {
                return Err(Error::InvalidText(
                    "cannot take logarithm of a non-positive number".into(),
                ));
            }
            x.ln()
        }
        "log" | "log10" if args.len() == 1 => {
            let x = f(0)?;
            if x <= 0.0 {
                return Err(Error::InvalidText(
                    "cannot take logarithm of a non-positive number".into(),
                ));
            }
            x.log10()
        }
        "log" => f(1)?.log(f(0)?),
        "power" | "pow" => f(0)?.powf(f(1)?),
        _ => return Err(Error::Unsupported(format!("function {name}()"))),
    };
    Ok(Bson::Double(out))
}

/// One character's SIMPLE uppercase: its full mapping when that is a single
/// character, else itself (`ß` stays `ß`).
fn simple_upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// One character's SIMPLE lowercase; `İ` (U+0130) is `i`, as towlower has it.
pub(crate) fn simple_lower(c: char) -> char {
    if c == '\u{130}' {
        return 'i';
    }
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// MD5, for `md5()`. Small enough to carry rather than take a dependency for.
/// PostgreSQL's `format()`: `%s` (text, NULL empty), `%I` (`quote_ident`,
/// NULL is an error), `%L` (`quote_literal`, NULL is the word `NULL`), `%%`,
/// each optionally positional (`%2$s`). Measured on 16: too few arguments is
/// `22023 too few arguments for format()`, an unknown specifier is
/// `22023 unrecognized format() type specifier "x"`, and `%I` of NULL is
/// `22004 null values cannot be formatted as an SQL identifier`.
fn pg_format(fmt: &str, args: &[Bson]) -> Result<String> {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut next_arg = 0usize;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(&after) = chars.peek() else {
            return Err(Error::InvalidParameter(
                "unterminated format() type specifier".into(),
            ));
        };
        if after == '%' {
            chars.next();
            out.push('%');
            continue;
        }
        // `%[n$][-][width | * | *n$]type`.
        let digits_of = |chars: &mut std::iter::Peekable<std::str::Chars>| {
            let mut d = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    d.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            d
        };
        let position = |n: &str| -> Result<usize> {
            let n: usize = n.parse().unwrap_or(0);
            if n == 0 {
                return Err(Error::InvalidParameter(
                    "format specifies argument 0, but arguments are numbered from 1".into(),
                ));
            }
            Ok(n - 1)
        };
        let mut digits = digits_of(&mut chars);
        let mut explicit_index = None;
        if !digits.is_empty() && chars.peek() == Some(&'$') {
            chars.next();
            explicit_index = Some(position(&digits)?);
            digits.clear();
        }
        let mut left = false;
        if digits.is_empty() {
            while chars.peek() == Some(&'-') {
                chars.next();
                left = true;
            }
        }
        let mut width: Option<i64> = None;
        if digits.is_empty() && chars.peek() == Some(&'*') {
            chars.next();
            let d = digits_of(&mut chars);
            let wi = if !d.is_empty() && chars.peek() == Some(&'$') {
                chars.next();
                position(&d)?
            } else {
                let i = next_arg;
                next_arg += 1;
                i
            };
            let w = args
                .get(wi)
                .ok_or_else(|| Error::InvalidParameter("too few arguments for format()".into()))?;
            width = match w {
                Bson::Null => None,
                Bson::Int32(v) => Some(i64::from(*v)),
                Bson::Int64(v) => Some(*v),
                other => {
                    let t = text(other);
                    Some(t.trim().parse().map_err(|_| {
                        Error::InvalidText(format!(
                            "invalid input syntax for type integer: \"{t}\""
                        ))
                    })?)
                }
            };
        } else {
            if digits.is_empty() {
                digits = digits_of(&mut chars);
            }
            if !digits.is_empty() {
                width = digits.parse().ok();
            }
        }
        if let Some(w) = width {
            if w < 0 {
                left = true;
                width = Some(-w);
            }
        }
        let index = match explicit_index {
            Some(i) => i,
            None => {
                let i = next_arg;
                next_arg += 1;
                i
            }
        };
        let Some(spec) = chars.next() else {
            return Err(Error::InvalidParameter(
                "unterminated format() type specifier".into(),
            ));
        };
        let value = args
            .get(index)
            .ok_or_else(|| Error::InvalidParameter("too few arguments for format()".into()))?;
        let field_start = out.len();
        match spec {
            's' => {
                if *value != Bson::Null {
                    out.push_str(&text(value));
                }
            }
            'I' => {
                if *value == Bson::Null {
                    return Err(Error::NullValueNotAllowed(
                        "null values cannot be formatted as an SQL identifier".into(),
                    ));
                }
                out.push_str(&quote_identifier(&text(value)));
            }
            'L' => {
                if *value == Bson::Null {
                    out.push_str("NULL");
                } else {
                    out.push_str(&quote_literal(&text(value)));
                }
            }
            other => {
                return Err(Error::InvalidParameter(format!(
                    "unrecognized format() type specifier \"{other}\""
                )));
            }
        }
        // Pad the field to its width, on the right when left-justified.
        if let Some(w) = width {
            let field: String = out[field_start..].to_string();
            let n = field.chars().count() as i64;
            if n < w {
                let pad = " ".repeat((w - n) as usize);
                out.truncate(field_start);
                if left {
                    out.push_str(&field);
                    out.push_str(&pad);
                } else {
                    out.push_str(&pad);
                    out.push_str(&field);
                }
            }
        }
    }
    Ok(out)
}

/// PostgreSQL's `quote_literal`: single quotes doubled, and a backslash
/// forces the `E'...'` form with the backslashes doubled too.
pub fn quote_literal(text: &str) -> String {
    let body = text.replace('\'', "''");
    if body.contains('\\') {
        format!("E'{}'", body.replace('\\', "\\\\"))
    } else {
        format!("'{body}'")
    }
}

/// PostgreSQL's `quote_identifier`: an identifier is left bare only when it
/// is all lower-case letters, digits and underscores, does not start with a
/// digit, AND is not a keyword of any category above UNRESERVED. Measured on
/// 16: `select`, `user`, `between`, `cross` and `order` are quoted, while
/// `int4`, `abort` and `zone` (unreserved) are not. Embedded double quotes
/// are doubled.
pub fn quote_identifier(ident: &str) -> String {
    let plain = !ident.is_empty()
        && ident
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !ident.chars().next().is_some_and(|c| c.is_ascii_digit());
    let safe = plain && !is_reserved_word(ident);
    if safe {
        ident.to_string()
    } else {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
}

/// Whether a plain lower-case word is a keyword PostgreSQL would quote: any
/// keyword category except UNRESERVED. The scanner classifies a lone word.
fn is_reserved_word(word: &str) -> bool {
    use pg_query::protobuf::KeywordKind;
    let Ok(scanned) = pg_query::scan(word) else {
        return false;
    };
    let [tok] = scanned.tokens.as_slice() else {
        return false;
    };
    matches!(
        KeywordKind::try_from(tok.keyword_kind),
        Ok(KeywordKind::ColNameKeyword
            | KeywordKind::TypeFuncNameKeyword
            | KeywordKind::ReservedKeyword)
    )
}

pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The RESULT type of a scalar, for the describe pass -- which sees no value
/// to infer from. The string functions answer text; the numeric ones vary by
/// input and fall back to text only when unknown, which is also what an
/// untyped output column defaults to.
pub fn static_result_type(name: &str) -> &'static str {
    if name == "secantus_hash_partition" || name == "__net_op" {
        return "bool";
    }
    if name == "__net_arith" {
        return "inet";
    }
    if name == "__net_diff" {
        return "int8";
    }
    if let Some(t) = crate::correlated::executor_function_type(name) {
        return t;
    }
    if matches!(
        name,
        "pg_relation_size"
            | "pg_total_relation_size"
            | "pg_table_size"
            | "pg_indexes_size"
            | "pg_database_size"
            | "pg_size_bytes"
    ) {
        return "int8";
    }
    if name == "pg_size_pretty" {
        return "text";
    }
    if matches!(name, "sha224" | "sha256" | "sha384" | "sha512") {
        return "bytea";
    }
    if name == "pg_column_size" {
        return "int4";
    }
    if let Some(t) = crate::arrays::static_result_type(name) {
        return t;
    }
    if let Some(t) = crate::fts::result_type(name) {
        return t;
    }
    if let Some(t) = crate::xml::result_type(name) {
        return t;
    }
    if let Some(t) = crate::jsonops::result_type(name) {
        return t;
    }
    if let Some(t) = crate::mathfn::result_type(name) {
        return t;
    }
    if crate::trgm::is_function(name) {
        if let Some(t) = crate::trgm::result_type(name) {
            return t;
        }
    }
    if let Some(t) = crate::pgcrypto::result_type(name) {
        return t;
    }
    if let Some(t) = crate::geom::result_type(name) {
        return t;
    }
    match name {
        "gen_random_uuid" | "uuid_generate_v4" => "uuid",
        "random" => "float8",
        "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
        | "strpos" | "position" | "ascii" | "get_byte" => "int4",
        "abs" | "ceil" | "ceiling" | "floor" | "round" | "trunc" | "mod" | "div" => "numeric",
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow" | "sign" => "float8",
        "scale" | "min_scale" => "int4",
        "current_schema" | "current_database" | "current_user" | "session_user" => "name",
        "trim_scale" => "numeric",
        "numeric_send" => "bytea",
        "starts_with" => "bool",
        n if n.starts_with("pg_") && n.ends_with("_is_visible") => "bool",
        "pg_relation_is_publishable" => "bool",
        "pg_get_userbyid" => "name",
        "pg_get_function_sqlbody" => "text",
        "to_number" => "numeric",
        "jsonb_path_exists"
        | "jsonb_path_match"
        | "jsonb_path_exists_tz"
        | "jsonb_path_match_tz"
        | "is_normalized"
        | "regexp_like" => "bool",
        "regexp_count" | "regexp_instr" => "int4",
        "regexp_substr" => "text",
        "jsonb_path_query_first"
        | "jsonb_path_query_array"
        | "jsonb_path_query_first_tz"
        | "jsonb_path_query_array_tz" => "jsonb",
        "lpad" | "rpad" | "to_hex" | "translate" | "overlay" | "quote_literal"
        | "quote_nullable" | "unistr" | "convert_from" | "normalize" => "text",
        "regexp_split_to_array" => "text[]",
        "set_byte" | "decode" => "bytea",
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            "timestamptz"
        }
        "st_geomfromgeojson" | "st_geomfromtext" | "st_geomfromewkt" => "geometry",
        "st_srid" => "int4",
        "hstore" | "delete" => "hstore",
        "akeys" | "avals" => "text[]",
        "exist" | "defined" => "bool",
        "hstore_to_json" => "json",
        "hstore_to_jsonb" => "jsonb",
        _ => "text",
    }
}

/// 64 random bits. `RandomState` is seeded from the operating system once per
/// process and stepped for every instance, so each call hashes to a fresh,
/// unpredictable value without a dependency on a random-number crate.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish()
}

/// `similar_escape_internal`: a SQL `SIMILAR TO` pattern as an anchored
/// POSIX regex. `%` is `.*`, `_` is `.`, `(` is non-capturing, the regex
/// metacharacters `\ . ^ $` are escaped, a bracket expression passes through,
/// and escape-double-quotes split the pattern into its three SUBSTRING parts.
fn similar_escape(pattern: &str, escape: Option<&str>) -> Result<String> {
    let esc: Option<char> = match escape {
        None => Some('\\'),
        Some("") => None,
        Some(e) => {
            if e.chars().count() > 1 {
                return Err(Error::Sqlstate("22025", "invalid escape string".into()));
            }
            e.chars().next()
        }
    };
    let mut r = String::from("^(?:");
    let mut after = false;
    let mut nquotes = 0;
    let mut depth = 0;
    let mut pos = 0;
    for c in pattern.chars() {
        if after {
            if c == '"' && depth < 1 {
                match nquotes {
                    0 => r.push_str("){1,1}?("),
                    1 => r.push_str("){1,1}(?:"),
                    _ => {
                        return Err(Error::Sqlstate(
                            "2200C",
                            "SQL regular expression may not contain more than two escape-double-quote separators"
                                .into(),
                        ))
                    }
                }
                nquotes += 1;
            } else {
                r.push('\\');
                r.push(c);
                pos = 3;
            }
            after = false;
        } else if Some(c) == esc {
            after = true;
        } else if depth > 0 {
            if c == '\\' {
                r.push('\\');
            }
            r.push(c);
            if c == ']' && pos > 2 {
                depth -= 1;
            } else if c == '[' {
                depth += 1;
                pos = 3;
            } else if c == '^' {
                pos += 1;
            } else {
                pos = 3;
            }
        } else if c == '[' {
            r.push(c);
            depth = 1;
            pos = 1;
        } else if c == '%' {
            r.push_str(".*");
        } else if c == '_' {
            r.push('.');
        } else if c == '(' {
            r.push_str("(?:");
        } else if matches!(c, '\\' | '.' | '^' | '$') {
            r.push('\\');
            r.push(c);
        } else {
            r.push(c);
        }
    }
    r.push_str(")$");
    Ok(r)
}

/// `pg_size_pretty(bigint | numeric)`, PostgreSQL 14's rule: bytes below
/// 10 kB, then each unit while the value stays under 20 of it (with a half
/// unit of rounding carried in one spare bit), up to TB.
fn size_pretty(v: &Bson) -> Result<Bson> {
    let size: i128 = match v {
        Bson::Int32(i) => i128::from(*i),
        Bson::Int64(i) => i128::from(*i),
        Bson::Double(d) => *d as i128,
        other => crate::value_text(other)
            .split('.')
            .next()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| Error::InvalidText("invalid size".into()))?,
    };
    let limit: i128 = 10 * 1024;
    let limit2 = limit * 2 - 1;
    let half_rounded = |x: i128| (x + if x < 0 { -1 } else { 1 }) / 2;
    if size.abs() < limit {
        return Ok(Bson::String(format!("{size} bytes")));
    }
    let mut s = size >> 9;
    for unit in ["kB", "MB", "GB"] {
        if s.abs() < limit2 {
            return Ok(Bson::String(format!("{} {unit}", half_rounded(s))));
        }
        s >>= 10;
    }
    Ok(Bson::String(format!("{} TB", half_rounded(s))))
}

/// `pg_size_bytes(text)`: a number, then optionally `bytes` / `kB` / `MB` /
/// `GB` / `TB` (any case, 1024-based).
fn size_bytes(text: &str) -> Result<Bson> {
    let t = text.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let invalid = || Error::InvalidText(format!("invalid size: \"{text}\""));
    let value: f64 = num.trim().parse().map_err(|_| invalid())?;
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "bytes" => 1.0,
        "kb" => 1024.0,
        "mb" => 1024.0 * 1024.0,
        "gb" => 1024.0 * 1024.0 * 1024.0,
        "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(Error::InvalidText(format!("invalid size: \"{text}\""))),
    };
    Ok(Bson::Int64((value * mult) as i64))
}

/// `pg_column_size(value)`: the bytes the value's datum takes -- a short
/// varlena (under 127 bytes) carries a 1-byte header, a longer one 4.
fn column_size(v: &Bson) -> Bson {
    match v {
        Bson::Null => Bson::Null,
        Bson::Boolean(_) => Bson::Int32(1),
        Bson::Int32(_) => Bson::Int32(4),
        Bson::Int64(_) | Bson::Double(_) | Bson::DateTime(_) => Bson::Int32(8),
        other => {
            let n = crate::value_text(other).len();
            Bson::Int32(if n < 127 { n + 1 } else { n + 4 } as i32)
        }
    }
}

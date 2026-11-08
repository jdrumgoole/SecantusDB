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
    SCALAR_NAMES.contains(&name) || extension_scalar(name).is_some()
}

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
    "lpad",
    "rpad",
    "reverse",
    "translate",
    "left",
    "right",
    "strpos",
    "position",
    "concat",
    "concat_ws",
    "md5",
    "quote_ident",
    "quote_literal",
    "quote_nullable",
    "to_json",
    "json_build_array",
    "jsonb_build_array",
    "json_build_object",
    "jsonb_build_object",
    "to_jsonb",
    "array_to_string",
    "array_length",
    "array_dims",
    "array_ndims",
    "array_upper",
    "array_lower",
    "cardinality",
    "format_type",
    "pg_get_constraintdef",
    "pg_get_expr",
    "pg_table_is_visible",
    "pg_get_userbyid",
    "pg_type_is_visible",
    "pg_function_is_visible",
    "pg_get_indexdef",
    "format",
    "chr",
    "ascii",
    "split_part",
    "starts_with",
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
    "transaction_timestamp",
    "statement_timestamp",
    "clock_timestamp",
    "gen_random_uuid",
    "random",
    "pi",
    "area",
    "width",
    "height",
    "bound_box",
];

/// The current instant as a stored `timestamptz` value.
///
/// PostgreSQL's `now()` is the TRANSACTION's start time and
/// `statement_timestamp()` the statement's; this planner evaluates a
/// constant when it plans the statement, so all four clocks read the
/// statement's own time. A transaction that reads `now()` twice sees two
/// values where PostgreSQL shows one.
pub fn now_value() -> Bson {
    crate::timestamptz_value_from_micros(crate::transaction_clock_micros())
}

fn text(v: &Bson) -> String {
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
        // Every other value takes its `::text` form -- never a Debug dump.
        other => crate::render_value_text(other),
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

/// Evaluate a scalar built-in. `None` means "not one of ours".
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_scalar(name) {
        return None;
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    // Most scalar built-ins are NULL-propagating: given a NULL argument the
    // answer is NULL, not an error. The exceptions IGNORE a NULL argument
    // instead: `concat` and `concat_ws` skip them, and `greatest` /
    // `least` pick the extreme of what remains, so `greatest(1, NULL)` is 1;
    // the JSON constructors carry it as a JSON `null` (and refuse it as a
    // key).
    // `format_type` is NULL only on a NULL oid: a NULL typmod means "no
    // modifier", so `format_type(23, NULL)` is `integer`.
    if !matches!(
        name,
        "concat"
            | "concat_ws"
            | "greatest"
            | "least"
            | "format"
            | "format_type"
            | "quote_nullable"
            | "array_to_string"
            | "json_build_array"
            | "jsonb_build_array"
            | "json_build_object"
            | "jsonb_build_object"
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
        // The box functions. A box argument arrives as its tagged document;
        // anything else matches no overload (42883). An UNTYPED argument is
        // refused before the call (`refuse_untyped_any_args`).
        "area" | "width" | "height" | "bound_box" => {
            need(if name == "bound_box" { 2 } else { 1 })?;
            let boxed = |i: usize| -> Result<[f64; 4]> {
                let v = arg(i);
                crate::geo::box_coords(&v).ok_or_else(|| {
                    let t = crate::display_type(crate::inferred_type(&v));
                    Error::UndefinedFunction(format!("function {name}({t}) does not exist"))
                })
            };
            let a = boxed(0)?;
            Ok(match name {
                "area" => Bson::Double((a[0] - a[2]) * (a[1] - a[3])),
                "width" => Bson::Double(a[0] - a[2]),
                "height" => Bson::Double(a[1] - a[3]),
                _ => {
                    let b = boxed(1)?;
                    crate::geo::box_value([
                        a[0].max(b[0]),
                        a[1].max(b[1]),
                        a[2].min(b[2]),
                        a[3].min(b[3]),
                    ])
                }
            })
        }
        // Three clocks, as PostgreSQL has them: the transaction's start, the
        // statement's start, and the wall clock at the call.
        "now" | "transaction_timestamp" => {
            need(0)?;
            Ok(now_value())
        }
        "statement_timestamp" => {
            need(0)?;
            Ok(crate::timestamptz_value_from_micros(
                crate::statement_clock_micros(),
            ))
        }
        "clock_timestamp" => {
            need(0)?;
            Ok(crate::timestamptz_value_from_micros(
                crate::wall_clock_micros(),
            ))
        }
        "pi" => {
            need(0)?;
            Ok(Bson::Double(std::f64::consts::PI))
        }
        // A uniform `float8` in [0, 1), as `random()` returns.
        "random" => {
            need(0)?;
            Ok(Bson::Double(rand::random::<f64>()))
        }
        // A version-4 UUID: 122 random bits under the version and variant
        // markers, rendered in the canonical lower-case form.
        "gen_random_uuid" => {
            need(0)?;
            let mut b: [u8; 16] = rand::random();
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            Ok(Bson::String(format!(
                "{}-{}-{}-{}-{}",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            )))
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
        "upper" => {
            need(1)?;
            Ok(Bson::String(s(0).to_uppercase()))
        }
        "lower" => {
            need(1)?;
            Ok(Bson::String(s(0).to_lowercase()))
        }
        "initcap" => {
            need(1)?;
            let mut out = String::new();
            let mut fresh = true;
            for c in s(0).chars() {
                if c.is_alphanumeric() {
                    if fresh {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
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
            // `length(bit)` is the number of bits.
            if let Some((digits, _)) = crate::bits::bit_digits(&arg(0)) {
                return Ok(Bson::Int32(digits.len() as i32));
            }
            Ok(Bson::Int32(s(0).chars().count() as i32))
        }
        "octet_length" => {
            need(1)?;
            if let Bson::Binary(b) = &arg(0) {
                return Ok(Bson::Int32(b.bytes.len() as i32));
            }
            if let Some((digits, _)) = crate::bits::bit_digits(&arg(0)) {
                return Ok(Bson::Int32(digits.len().div_ceil(8) as i32));
            }
            Ok(Bson::Int32(s(0).len() as i32))
        }
        "bit_length" => {
            need(1)?;
            if let Bson::Binary(b) = &arg(0) {
                return Ok(Bson::Int32((b.bytes.len() * 8) as i32));
            }
            if let Some((digits, _)) = crate::bits::bit_digits(&arg(0)) {
                return Ok(Bson::Int32(digits.len() as i32));
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
            let start = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            let len = if args.len() == 3 {
                let l = as_i64(&arg(2)).ok_or_else(|| wrong_args(name))?;
                if l < 0 {
                    return Err(Error::InvalidText(
                        "negative substring length not allowed".into(),
                    ));
                }
                Some(l)
            } else {
                None
            };
            // `substring(bit ...)` is a bit string of the digits taken.
            if let Some((digits, varying)) = crate::bits::bit_digits(&arg(0)) {
                return Ok(crate::bits::bit_value(
                    substring(digits, start, len),
                    varying,
                ));
            }
            Ok(Bson::String(substring(&s(0), start, len)))
        }
        "replace" => {
            need(3)?;
            Ok(Bson::String(s(0).replace(&s(1), &s(2))))
        }
        "lpad" | "rpad" => {
            if !(2..=3).contains(&args.len()) {
                return Err(wrong_args(name));
            }
            let chars: Vec<char> = s(0).chars().collect();
            let length = usize::try_from(as_i64(&arg(1)).unwrap_or(0)).unwrap_or(0);
            let fill: Vec<char> = if args.len() == 3 {
                s(2).chars().collect()
            } else {
                vec![' ']
            };
            // Too long is truncated to `length`; too short is padded with
            // `fill` cycled, unless `fill` is empty (nothing to pad with).
            if chars.len() >= length || fill.is_empty() {
                let kept: String = chars.iter().take(length).collect();
                return Ok(Bson::String(kept));
            }
            let padding: String = fill.iter().cycle().take(length - chars.len()).collect();
            let text: String = chars.iter().collect();
            Ok(Bson::String(if name == "lpad" {
                padding + &text
            } else {
                text + &padding
            }))
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
        // `translate(s, from, to)`: each character of `from` maps to the
        // character at the same position in `to`, or is dropped when `to`
        // is shorter; the FIRST occurrence in `from` decides (`translate(
        // 'aaa', 'aa', 'xy')` is `xxx`). Measured 16.
        "translate" => {
            need(3)?;
            let from: Vec<char> = s(1).chars().collect();
            let to: Vec<char> = s(2).chars().collect();
            let out: String = s(0)
                .chars()
                .filter_map(|c| match from.iter().position(|f| *f == c) {
                    Some(i) => to.get(i).copied(),
                    None => Some(c),
                })
                .collect();
            Ok(Bson::String(out))
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
        "md5" => {
            need(1)?;
            Ok(Bson::String(md5_hex(s(0).as_bytes())))
        }
        "quote_ident" => {
            need(1)?;
            Ok(Bson::String(quote_identifier(&s(0))))
        }
        "quote_literal" => {
            need(1)?;
            Ok(Bson::String(quote_literal(&s(0))))
        }
        // `quote_nullable(NULL)` is the bare word NULL, not a NULL answer.
        "quote_nullable" => {
            need(1)?;
            Ok(Bson::String(match arg(0) {
                Bson::Null => "NULL".to_string(),
                v => quote_literal(&text(&v)),
            }))
        }
        "to_json" | "to_jsonb" => {
            need(1)?;
            Ok(Bson::String(if name == "to_json" {
                json_text_of(name, &arg(0))?
            } else {
                crate::json::render_jsonb(&json_of(name, &arg(0))?)
            }))
        }
        // The constructors. The `json` forms keep each element's own text
        // (a `json` argument verbatim, everything else compact) and join
        // with `, ` -- `[1, "a"]`, `{"k" : 1}` -- where the `jsonb` forms
        // normalise the whole value (measured 16).
        "json_build_array" => {
            let items = args
                .iter()
                .map(|v| json_text_of(name, v))
                .collect::<Result<Vec<_>>>()?;
            Ok(Bson::String(format!("[{}]", items.join(", "))))
        }
        "jsonb_build_array" => {
            let items = args
                .iter()
                .map(|v| json_of(name, v))
                .collect::<Result<Vec<_>>>()?;
            Ok(Bson::String(crate::json::render_jsonb(
                &crate::json::Json::Array(items),
            )))
        }
        "json_build_object" | "jsonb_build_object" => {
            if !args.len().is_multiple_of(2) {
                return Err(Error::InvalidParameter(
                    "argument list must have even number of elements".into(),
                ));
            }
            let mut pairs = Vec::with_capacity(args.len() / 2);
            for kv in args.chunks(2) {
                let key = match &kv[0] {
                    Bson::Null => {
                        return Err(Error::NullValueNotAllowed(
                            "null value not allowed for object key".into(),
                        ))
                    }
                    Bson::Array(_) | Bson::Document(_) => {
                        return Err(Error::InvalidParameter(
                            "key value must be scalar, not array, composite, or json".into(),
                        ))
                    }
                    // A boolean key is spelled `true` / `false`, not `t` / `f`.
                    Bson::Boolean(b) => b.to_string(),
                    k => text(k),
                };
                pairs.push((key, &kv[1]));
            }
            Ok(Bson::String(if name == "json_build_object" {
                let members = pairs
                    .iter()
                    .map(|(k, v)| {
                        Ok(format!(
                            "{} : {}",
                            crate::json::quote_string(k),
                            json_text_of(name, v)?
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                format!("{{{}}}", members.join(", "))
            } else {
                let members = pairs
                    .into_iter()
                    .map(|(k, v)| Ok((k, json_of(name, v)?)))
                    .collect::<Result<Vec<_>>>()?;
                crate::json::render_jsonb(&crate::json::Json::Object(members))
            }))
        }
        // `array_to_string(arr, sep[, null_string])`: a NULL array or
        // separator is a NULL answer; NULL elements are skipped unless the
        // third argument names their spelling.
        "array_to_string" => {
            if !(args.len() == 2 || args.len() == 3) {
                return Err(wrong_args(name));
            }
            let (items, sep) = match (arg(0), arg(1)) {
                (Bson::Null, _) | (_, Bson::Null) => return Ok(Bson::Null),
                (Bson::Array(items), sep) => (items, text(&sep)),
                _ => return Err(wrong_args(name)),
            };
            let null_text = match args.get(2) {
                None | Some(Bson::Null) => None,
                Some(v) => Some(text(v)),
            };
            let parts: Vec<String> = items
                .iter()
                .filter_map(|v| match v {
                    Bson::Null => null_text.clone(),
                    v => Some(text(v)),
                })
                .collect();
            Ok(Bson::String(parts.join(&sep)))
        }
        // The array-shape functions. Every stored array has a lower bound of
        // 1 (a `[2:3]=` decoration is not kept), so the bounds are the
        // lengths; an empty array has no dimensions and answers NULL to all
        // of them but `cardinality`.
        "array_dims" | "array_ndims" | "cardinality" => {
            need(1)?;
            let Bson::Array(items) = arg(0) else {
                return Err(wrong_args(name));
            };
            let dims = array_shape(&items);
            Ok(match name {
                "cardinality" if dims.is_empty() => Bson::Int32(0),
                "cardinality" => Bson::Int32(dims.iter().product::<usize>() as i32),
                _ if dims.is_empty() => Bson::Null,
                "array_ndims" => Bson::Int32(dims.len() as i32),
                _ => Bson::String(dims.iter().map(|n| format!("[1:{n}]")).collect()),
            })
        }
        "array_length" | "array_upper" | "array_lower" => {
            need(2)?;
            let Bson::Array(items) = arg(0) else {
                return Err(wrong_args(name));
            };
            let dim = as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?;
            let dims = array_shape(&items);
            let Some(n) = usize::try_from(dim)
                .ok()
                .filter(|d| *d >= 1)
                .and_then(|d| dims.get(d - 1))
            else {
                return Ok(Bson::Null);
            };
            Ok(Bson::Int32(if name == "array_lower" {
                1
            } else {
                *n as i32
            }))
        }
        "format_type" => {
            need(2)?;
            let oid = match arg(0) {
                Bson::Null => return Ok(Bson::Null),
                v => crate::regtype_oid(&v)
                    .or_else(|| as_i64(&v))
                    .ok_or_else(|| wrong_args(name))?,
            };
            let typmod = match arg(1) {
                Bson::Null => None,
                v => Some(
                    as_i64(&v)
                        .and_then(|m| i32::try_from(m).ok())
                        .ok_or_else(|| wrong_args(name))?,
                ),
            };
            crate::format_type_text(oid, typmod).map(Bson::String)
        }
        // The catalog installs each constraint's / index's rendered
        // definition per statement; an oid nothing carries is NULL, as
        // PostgreSQL answers for one it cannot find (measured 16.15).
        // `pg_get_constraintdef(oid, pretty)` and `pg_get_indexdef(oid,
        // colno, pretty)` are the three-argument forms psql's `\d` uses;
        // `pretty` changes nothing in a definition this simple, and a
        // `colno` above 0 answers the bare expression of that index column
        // (`c`, not `c DESC`), or `''` past the last one (measured 16.15).
        "pg_get_constraintdef" | "pg_get_indexdef" => {
            let is_index = name == "pg_get_indexdef";
            let extra = if is_index { 2 } else { 1 };
            if args.len() != 1 && args.len() != 1 + extra {
                return Err(wrong_args(name));
            }
            if args.contains(&Bson::Null) {
                return Ok(Bson::Null);
            }
            let oid = as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let text = if is_index {
                crate::index_def(oid)
            } else {
                crate::constraint_def(oid)
            };
            let Some(text) = text else {
                return Ok(Bson::Null);
            };
            let colno = if is_index && args.len() == 3 {
                as_i64(&arg(1)).ok_or_else(|| wrong_args(name))?
            } else {
                0
            };
            if colno <= 0 {
                // `pretty` drops the schema of a table on the search path
                // (PostgreSQL 16: `ON qa` for `public.qa`, `ON qt` for
                // `pg_temp.qt`); the unpretty form always qualifies.
                let pretty = is_index && args.len() == 3 && arg(2) == Bson::Boolean(true);
                if pretty {
                    if let Some((head, tail)) = text.split_once(" ON ") {
                        let unqualified = tail
                            .strip_prefix("public.")
                            .or_else(|| tail.strip_prefix("pg_temp."))
                            .unwrap_or(tail);
                        return Ok(Bson::String(format!("{head} ON {unqualified}")));
                    }
                }
                return Ok(Bson::String(text));
            }
            let columns = text
                .rsplit_once(" (")
                .and_then(|(_, tail)| tail.strip_suffix(')'))
                .unwrap_or("");
            let column = columns
                .split(", ")
                .nth(colno as usize - 1)
                .map(|c| c.strip_suffix(" DESC").unwrap_or(c))
                .unwrap_or("");
            Ok(Bson::String(column.to_string()))
        }
        // `pg_get_expr(expr, relid [, pretty])`: `pg_attrdef.adbin` and
        // `pg_constraint.conbin` already hold rendered SQL text, so the
        // decompilation is the identity.
        "pg_get_expr" => {
            if args.len() != 2 && args.len() != 3 {
                return Err(wrong_args(name));
            }
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            Ok(Bson::String(s(0)))
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
        "split_part" => {
            need(3)?;
            let n = as_i64(&arg(2)).unwrap_or(0);
            let subject = s(0);
            let sep = s(1);
            // An empty delimiter makes the whole string field one; a
            // negative position counts from the end (PostgreSQL 16).
            let parts: Vec<&str> = if sep.is_empty() {
                vec![subject.as_str()]
            } else {
                subject.split(&sep as &str).collect()
            };
            let idx = if n > 0 {
                usize::try_from(n - 1).ok()
            } else if n < 0 {
                usize::try_from(n.unsigned_abs())
                    .ok()
                    .and_then(|k| parts.len().checked_sub(k))
            } else {
                return Err(Error::InvalidParameter(
                    "field position must not be zero".into(),
                ));
            };
            Ok(Bson::String(
                idx.and_then(|i| parts.get(i))
                    .copied()
                    .unwrap_or("")
                    .to_string(),
            ))
        }
        // Every schema this server has is on the search path (`public`,
        // `pg_temp`, `pg_catalog`), so any existing object is visible; a
        // NULL oid is a NULL answer.
        "pg_table_is_visible" | "pg_type_is_visible" | "pg_function_is_visible" => {
            need(1)?;
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            Ok(Bson::Boolean(true))
        }
        // A role's name by oid; one nothing carries is `unknown (OID=n)`
        // (measured 16.15).
        "pg_get_userbyid" => {
            need(1)?;
            if arg(0) == Bson::Null {
                return Ok(Bson::Null);
            }
            let oid = as_i64(&arg(0)).ok_or_else(|| wrong_args(name))?;
            let name = crate::role_name(oid).unwrap_or_else(|| match oid {
                // The pseudo-role that owns `public`.
                6171 => "pg_database_owner".to_string(),
                _ => format!("unknown (OID={oid})"),
            });
            Ok(Bson::String(name))
        }
        "starts_with" => {
            need(2)?;
            Ok(Bson::Boolean(s(0).starts_with(&s(1))))
        }
        // --- numeric -------------------------------------------------------
        // `abs` gives back the type it was handed, so an exact numeric stays
        // exact rather than becoming a float.
        "abs" => {
            need(1)?;
            no_money_function(name, &arg(0))?;
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
        "ceil" | "ceiling" | "floor" | "trunc" | "round" => {
            if args.is_empty() {
                return Err(wrong_args(name));
            }
            no_money_function(name, &arg(0))?;
            numeric_rounding(name, args)
        }
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow" => float_math(name, args),
        "mod" => {
            need(2)?;
            match (arg(0), arg(1)) {
                // `mod(numeric, numeric)` is the numeric `%`: `mod(7.5, 2)`
                // is `1.5`, not the integer remainder.
                (a, b) if crate::is_numeric(&a) || crate::is_numeric(&b) => {
                    let text = crate::numeric::numeric_operand_text;
                    match (text(&a), text(&b)) {
                        (Some(x), Some(y)) => crate::numeric::decimal_arith("%", &x, &y)
                            .unwrap_or_else(|| Err(wrong_args(name))),
                        _ => Err(wrong_args(name)),
                    }
                }
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
            Ok(best.unwrap_or(Bson::Null))
        }
        _ => Err(Error::Unsupported(format!("function {name}()"))),
    }
}

/// `ceil` / `floor` / `trunc` / `round`, which keep an exact input exact.
///
/// `round(numeric)` goes half AWAY FROM ZERO and `round(float8)` goes half TO
/// EVEN — the same split the integer casts have, and the same trap.
/// The numeric functions have no money overload: `abs(money)` is 42883
/// `function abs(money) does not exist` (measured 16).
fn no_money_function(name: &str, arg: &Bson) -> Result<()> {
    if crate::money::money_cents(arg).is_some() {
        return Err(Error::UndefinedFunction(format!(
            "function {name}(money) does not exist"
        )));
    }
    Ok(())
}

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
        Bson::Int32(_) | Bson::Int64(_) => subject,
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
        // An optional `n$` position.
        let mut digits = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        let index = if !digits.is_empty() && chars.peek() == Some(&'$') {
            chars.next();
            let n: usize = digits.parse().map_err(|_| {
                Error::InvalidParameter(
                    "format specifies argument 0, but arguments are numbered from 1".into(),
                )
            })?;
            if n == 0 {
                return Err(Error::InvalidParameter(
                    "format specifies argument 0, but arguments are numbered from 1".into(),
                ));
            }
            n - 1
        } else {
            if !digits.is_empty() {
                // A width, which this server does not lay out.
                return Err(Error::Unsupported("a format() field width".into()));
            }
            let i = next_arg;
            next_arg += 1;
            i
        };
        let Some(spec) = chars.next() else {
            return Err(Error::InvalidParameter(
                "unterminated format() type specifier".into(),
            ));
        };
        let value = args
            .get(index)
            .ok_or_else(|| Error::InvalidParameter("too few arguments for format()".into()))?;
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
    }
    Ok(out)
}

/// A value as JSON, the way `to_json` sees it: numbers keep their text,
/// booleans and strings map directly, an array maps element-wise. A
/// date/time value cannot be told from its type here (`to_json` renders a
/// `timestamp` in ISO 8601 with a `T`, which the text form does not), so
/// those are refused rather than rendered wrong.
/// `v` as `json` text: a `json` argument verbatim (`to_json` of a `json`
/// is the identity, whitespace and all), anything else compact.
fn json_text_of(name: &str, v: &Bson) -> Result<String> {
    match crate::json::raw_text(v) {
        Some(text) => Ok(text.to_string()),
        None => Ok(crate::json::render_json_compact(&json_of(name, v)?)),
    }
}

fn json_of(name: &str, v: &Bson) -> Result<crate::json::Json> {
    use crate::json::Json;
    if let Some(text) = crate::json::raw_text(v) {
        return crate::json::parse(text)
            .map_err(|e| Error::Internal(format!("stored json failed to parse: {e:?}")));
    }
    Ok(match v {
        Bson::Null => Json::Null,
        // A bytea's output form, `\x0102`, as a string.
        Bson::Binary(_) => Json::Str(text(v)),
        Bson::Boolean(b) => Json::Bool(*b),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
            Json::Number(crate::render_value_text(v))
        }
        v if crate::is_wide_numeric(v) => Json::Number(text(v)),
        Bson::String(s) => Json::Str(s.clone()),
        Bson::Array(items) => Json::Array(
            items
                .iter()
                .map(|i| json_of(name, i))
                .collect::<Result<Vec<_>>>()?,
        ),
        Bson::DateTime(_) => {
            return Err(Error::Unsupported(format!("{name} over a date/time value")))
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "{name} over a {} value",
                crate::display_type(crate::inferred_type(v))
            )))
        }
    })
}

/// PostgreSQL's `quote_literal`: single quotes doubled, and a backslash
/// forces the `E'...'` form with the backslashes doubled too.
fn quote_literal(text: &str) -> String {
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

fn md5_hex(data: &[u8]) -> String {
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

/// The length of each dimension of an array: `[]` for an empty one, `[3]`
/// for `{1,2,3}`, `[2, 2]` for `{{1,2},{3,4}}`. Nested arrays are
/// rectangular by construction, so the first element's shape is every
/// element's.
fn array_shape(items: &[Bson]) -> Vec<usize> {
    let mut dims = Vec::new();
    let mut level = items;
    loop {
        if level.is_empty() {
            return dims;
        }
        dims.push(level.len());
        match level.first() {
            Some(Bson::Array(inner)) => level = inner,
            _ => return dims,
        }
    }
}

/// The RESULT type of a scalar, for the describe pass -- which sees no value
/// to infer from. The string functions answer text; the numeric ones vary by
/// input and fall back to text only when unknown, which is also what an
/// untyped output column defaults to.
/// Whether a result of the declared type is carried by a value that reads
/// as some OTHER type: a `timestamptz` instant is stored like a naive
/// `timestamp`, and `json` / `jsonb` are carried as their text.
pub fn value_hides_type(declared: &str) -> bool {
    matches!(declared, "timestamptz" | "json" | "jsonb" | "uuid" | "name")
}

pub fn static_result_type(name: &str) -> &'static str {
    known_result_type(name).unwrap_or("text")
}

/// The declared result type of a scalar function, or `None` for one this
/// table does not name -- which `static_result_type` reads as `text`, a
/// default that is right for the string functions and wrong for
/// `greatest` / `least`, whose type is their arguments'. A caller judging a
/// NULL-valued plan-time result needs to know the difference.
pub fn known_result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
        | "strpos" | "position" | "ascii" | "get_byte" => "int4",
        "abs" | "ceil" | "ceiling" | "floor" | "round" | "trunc" | "mod" | "div" => "numeric",
        "sqrt" | "exp" | "ln" | "log" | "log10" | "power" | "pow" | "sign" => "float8",
        "starts_with" | "pg_table_is_visible" | "pg_type_is_visible" | "pg_function_is_visible" => {
            "bool"
        }
        "set_byte" | "decode" => "bytea",
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            "timestamptz"
        }
        "gen_random_uuid" => "uuid",
        "pg_get_userbyid" => "name",
        "random" | "pi" => "float8",
        "st_geomfromgeojson" | "st_geomfromtext" | "st_geomfromewkt" => "geometry",
        "st_srid" => "int4",
        "area" | "width" | "height" => "float8",
        "bound_box" => "box",
        "hstore" | "delete" => "hstore",
        "akeys" | "avals" => "text[]",
        "exist" | "defined" => "bool",
        "hstore_to_json" | "to_json" | "json_build_array" | "json_build_object" => "json",
        "hstore_to_jsonb" | "to_jsonb" | "jsonb_build_array" | "jsonb_build_object" => "jsonb",
        "upper"
        | "lower"
        | "initcap"
        | "btrim"
        | "trim"
        | "ltrim"
        | "rtrim"
        | "substr"
        | "substring"
        | "replace"
        | "repeat"
        | "lpad"
        | "rpad"
        | "reverse"
        | "translate"
        | "left"
        | "right"
        | "concat"
        | "concat_ws"
        | "md5"
        | "quote_ident"
        | "quote_literal"
        | "quote_nullable"
        | "array_to_string"
        | "format_type"
        | "format"
        | "chr"
        | "split_part"
        | "encode"
        | "array_dims"
        | "pg_get_constraintdef"
        | "pg_get_indexdef"
        | "pg_get_expr" => "text",
        "array_length" | "array_ndims" | "array_upper" | "array_lower" | "cardinality" => "int4",
        _ => return None,
    })
}

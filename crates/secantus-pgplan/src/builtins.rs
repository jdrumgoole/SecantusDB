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

pub(crate) use crate::builtins_regex::{
    check_regex_flags, check_regex_numbers, regex_flags_position,
};
use crate::builtins_regex::{regexp_match, regexp_replace_from};
use crate::builtins_text::{
    decode, encode, encoding, encoding_by_name, invalid_utf8, like_escape, no_table, parse_ident,
    single_byte, to_ascii, Encoding,
};
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
        ("to_ascii", 1) => "text",
        ("age", 1) if t(0) == "xid" => "int4",
        ("mxid_age", 1) => "int4",
        ("int8_sum", 2) => "numeric",
        // A populate function's value is of its base argument's row type.
        ("json_populate_record" | "jsonb_populate_record", 2 | 3)
            if crate::user_composite(t(0)).is_some() =>
        {
            return Some(t(0).to_string());
        }
        // A built-in scalar base is no row type: the call says so (42804).
        ("json_populate_record" | "jsonb_populate_record", 2 | 3)
            if crate::pgtypes::oid_of_name(t(0)).is_some() && !t(0).ends_with("[]") =>
        {
            "record"
        }
        // A discrete range type's canonical function, called by name.
        ("int4range_canonical", 1) => "int4range",
        ("int8range_canonical", 1) => "int8range",
        ("daterange_canonical", 1) => "daterange",
        ("range_merge", 1) if crate::range::is_multirange_type(t(0)) => {
            return crate::range::multirange_member(t(0));
        }
        ("range_merge", 2) if crate::range::is_range_type(t(0)) && t(1) == t(0) => {
            return Some(t(0).to_string());
        }
        (
            "int4range_subdiff" | "int8range_subdiff" | "numrange_subdiff" | "daterange_subdiff"
            | "tsrange_subdiff" | "tstzrange_subdiff",
            2,
        ) => "float8",
        ("like_escape", 2) if bytea_first => "bytea",
        ("like_escape", 2) => "text",
        // `datetime_timestamp` / `datetime_timestamptz` /
        // `datetimetz_timestamptz`: a day and a time of day.
        ("timestamp", 2) if t(0) == "date" && t(1) == "time" => "timestamp",
        ("timestamptz", 2) if t(0) == "date" && matches!(t(1), "time" | "timetz") => "timestamptz",
        // The FUNCTION `pg_lsn(numeric)`; there is no such cast.
        ("pg_lsn", 1) if t(0) == "numeric" || matches!(t(0), "int2" | "int4" | "int8") => "pg_lsn",
        ("to_ascii", 2) if t(1) != "bytea" => "text",
        ("num_nulls" | "num_nonnulls", n) if n >= 1 => "int4",
        ("trunc", 1) if t(0) == "macaddr" => "macaddr",
        ("trunc", 1) if t(0) == "macaddr8" => "macaddr8",
        ("macaddr8_set7bit", 1) => "macaddr8",
        ("overlaps", 4) if types.iter().all(|t| period_type(t)) => "bool",
        ("to_tsvector", 1 | 2) if matches!(t(types.len() - 1), "json" | "jsonb") => "tsvector",
        ("json_to_tsvector" | "jsonb_to_tsvector", 2 | 3) => "tsvector",
        // `ts_headline` over a document answers the document, of its type.
        ("ts_headline", 2..=4) if matches!(t(0), "json" | "jsonb") => {
            return Some(t(0).to_string())
        }
        ("ts_headline", 3 | 4) if matches!(t(1), "json" | "jsonb") => {
            return Some(t(1).to_string())
        }
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
    !matches!(
        name,
        "num_nulls"
            | "num_nonnulls"
            | "overlaps"
            | "json_populate_record"
            | "jsonb_populate_record"
            | "int8_sum"
    )
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

pub(crate) fn int(v: &Bson, what: &str) -> Result<i64> {
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

pub(crate) fn text(v: &Bson) -> String {
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

/// An `xid` argument's 32-bit value.
fn xid_arg(v: &Bson) -> Result<u32> {
    crate::value_text(v)
        .trim()
        .parse::<u32>()
        .map_err(|_| Error::Unsupported("this transaction id".into()))
}

/// `range_union_internal` without its gap check: the lower bound that is
/// lower and the upper that is higher; at equal values an inclusive bound
/// wins. An empty range adds nothing.
fn range_hull(
    a: &crate::range::Range,
    b: &crate::range::Range,
    element: &str,
) -> Result<crate::range::Range> {
    use std::cmp::Ordering;
    if a.empty {
        return Ok(b.clone());
    }
    if b.empty {
        return Ok(a.clone());
    }
    let pick = |x: &Option<String>,
                xi: bool,
                y: &Option<String>,
                yi: bool,
                want: Ordering|
     -> Result<(Option<String>, bool)> {
        Ok(match (x, y) {
            (None, _) => (None, false),
            (_, None) => (None, false),
            (Some(xv), Some(yv)) => match crate::range::compare_bounds(xv, yv, element)? {
                Ordering::Equal => (x.clone(), xi || yi),
                o if o == want => (x.clone(), xi),
                _ => (y.clone(), yi),
            },
        })
    };
    let (lower, lower_inc) = pick(&a.lower, a.lower_inc, &b.lower, b.lower_inc, Ordering::Less)?;
    let (upper, upper_inc) = pick(
        &a.upper,
        a.upper_inc,
        &b.upper,
        b.upper_inc,
        Ordering::Greater,
    )?;
    Ok(crate::range::Range {
        empty: false,
        lower,
        lower_inc,
        upper,
        upper_inc,
    })
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
                // A multibyte encoding's characters need its own rules to
                // count, past the ASCII range every one of them shares.
                Encoding::Other(canonical) if !single_byte(canonical) && !value.is_ascii() => {
                    return Err(no_table(canonical))
                }
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
        // `to_ascii(text)` reads the text in the database's encoding, UTF8;
        // the encoding given by name is 42704 when PostgreSQL has none of
        // that name, and by number when no encoding has that number.
        // `xid_age`: transactions since `xid`, as the server counts them.
        "age" => Ok(Bson::Int32(crate::snapshots::xid_age(xid_arg(&args[0])?))),
        // `mxid_age`: against the next multixact id, which is 1 here for
        // good -- this server makes no multixacts. 0 is not a valid one.
        "mxid_age" => {
            let mxid = xid_arg(&args[0])?;
            if mxid == 0 {
                return Ok(Bson::Int32(i32::MAX));
            }
            Ok(Bson::Int32(1u32.wrapping_sub(mxid) as i32))
        }
        // `numeric_pg_lsn`: rounded half away from zero, then range-checked,
        // as `pg_lsn + numeric` from 0/0 is.
        "pg_lsn" => {
            if crate::value_text(&args[0]).trim() == "NaN" {
                return Err(Error::FeatureNotSupported(
                    "cannot convert NaN to pg_lsn".into(),
                ));
            }
            crate::systypes::lsn_arith(
                "+",
                &crate::systypes::lsn_bson(0),
                "pg_lsn",
                &args[0],
                "numeric",
            )
            .unwrap_or_else(|| Err(Error::Unsupported("pg_lsn(numeric)".into())))
        }
        // The day and the time of day read together, as `date + time` is: a
        // `time` in the session's zone, a `timetz` in its own. An infinite
        // day is that infinity.
        "timestamp" | "timestamptz" => {
            let day = crate::value_text(&args[0]);
            if day.ends_with("infinity") {
                return crate::cast_value(Bson::String(day), name);
            }
            let at = format!("{day} {}", crate::value_text(&args[1]));
            crate::cast_value(Bson::String(at), name)
        }
        "like_escape" if types.first().is_some_and(|t| t == "bytea") => Ok(bytea::to_binary(
            like_escape(&bytes(0)?, &bytes(1)?, b'\\')?,
        )),
        "like_escape" => {
            let p: Vec<char> = text(&args[0]).chars().collect();
            let e: Vec<char> = text(&args[1]).chars().collect();
            Ok(Bson::String(
                like_escape(&p, &e, '\\')?.into_iter().collect(),
            ))
        }
        "int4range_canonical" | "int8range_canonical" | "daterange_canonical" => {
            let ty = name.trim_end_matches("_canonical");
            let r = crate::range::from_text(&text(&args[0]), ty)?;
            Ok(Bson::String(crate::range::render(&r)))
        }
        // `range_merge(a, b)`: the smallest range holding both, gap or no gap.
        "range_merge" if args.len() == 2 => {
            let ty = types.first().map_or("", String::as_str);
            let a = crate::range::from_text(&text(&args[0]), ty)?;
            let b = crate::range::from_text(&text(&args[1]), ty)?;
            let element = crate::range::range_element(ty)
                .map(|(e, _)| e)
                .ok_or_else(|| Error::Unsupported(format!("range_merge over {ty}")))?;
            Ok(Bson::String(crate::range::render(&range_hull(
                &a, &b, &element,
            )?)))
        }
        // `range_merge(multirange)`: the smallest range holding every member.
        "range_merge" => {
            let ty = types.first().map_or("", String::as_str);
            let members = crate::range::multirange_from_text(&text(&args[0]), ty)?;
            let merged = match (members.first(), members.last()) {
                (Some(first), Some(last)) => crate::range::Range {
                    empty: false,
                    lower: first.lower.clone(),
                    lower_inc: first.lower_inc,
                    upper: last.upper.clone(),
                    upper_inc: last.upper_inc,
                },
                _ => crate::range::Range::empty(),
            };
            Ok(Bson::String(crate::range::render(&merged)))
        }
        // The `subtype_diff` functions: `a - b` as a float8 -- days for a
        // date, seconds for a timestamp.
        "int4range_subdiff" | "int8range_subdiff" => Ok(Bson::Double(
            int(&args[0], "bigint")? as f64 - int(&args[1], "bigint")? as f64,
        )),
        "numrange_subdiff" => {
            let diff = crate::eval_binary("-", args[0].clone(), args[1].clone())?;
            crate::cast_value(diff, "float8")
        }
        "daterange_subdiff" | "tsrange_subdiff" | "tstzrange_subdiff" => {
            // An untyped literal is read as the subtype first.
            let subtype = match name {
                "daterange_subdiff" => "date",
                "tsrange_subdiff" => "timestamp",
                _ => "timestamptz",
            };
            let typed = |i: usize| -> Result<Bson> {
                match types.get(i).map(String::as_str) {
                    Some(t) if t == subtype => Ok(args[i].clone()),
                    _ => crate::cast_value(args[i].clone(), subtype),
                }
            };
            let diff = crate::eval_binary("-", typed(0)?, typed(1)?)?;
            if name == "daterange_subdiff" {
                return crate::cast_value(diff, "float8");
            }
            let iv = crate::Interval::from_bson(&diff)
                .ok_or_else(|| Error::Unsupported(format!("{name} over these values")))?;
            Ok(Bson::Double(
                (i64::from(iv.days) * 86_400_000_000 + iv.micros) as f64 / 1_000_000.0,
            ))
        }
        "json_populate_record" | "jsonb_populate_record" => crate::populate_composite(
            name,
            &args[0],
            types.first().map_or("", String::as_str),
            &args[1],
        ),
        // `int8_sum(state, value)`, `sum(int8)`'s old transition function:
        // a NULL on either side is the other.
        "int8_sum" => match (&args[0], &args[1]) {
            (Bson::Null, Bson::Null) => Ok(Bson::Null),
            (Bson::Null, v) => crate::cast_value(v.clone(), "numeric"),
            (s, Bson::Null) => Ok(s.clone()),
            (s, v) => crate::eval_binary("+", s.clone(), crate::cast_value(v.clone(), "numeric")?),
        },
        "to_ascii" => {
            let canonical = match (args.get(1), types.get(1).map(String::as_str)) {
                (None, _) => "UTF8",
                (Some(v), Some("int2" | "int4" | "int8")) => {
                    let n = int(v, "integer")?;
                    usize::try_from(n)
                        .ok()
                        .and_then(|i| crate::scalar::PG_ENCODINGS.get(i).copied())
                        .ok_or_else(|| {
                            Error::Sqlstate("42704", format!("{n} is not a valid encoding code"))
                        })?
                }
                (Some(v), _) => {
                    let name = text(v);
                    encoding_by_name(&name).ok_or_else(|| {
                        Error::Sqlstate("42704", format!("{name} is not a valid encoding name"))
                    })?
                }
            };
            Ok(Bson::String(to_ascii(&text(&args[0]), canonical)?))
        }
        "convert_to" => {
            let (enc, shown) = encoding(&text(&args[1]), "destination ")?;
            Ok(bytea::to_binary(encode(&text(&args[0]), &enc, &shown)?))
        }
        "convert" => {
            let (from, from_name) = encoding(&text(&args[1]), "source ")?;
            let (to, shown) = encoding(&text(&args[2]), "destination ")?;
            // `pg_convert` hands back the bytes unchanged between one
            // encoding and itself, once they are checked as that encoding.
            if from_name == shown {
                if let Encoding::Utf8 = from {
                    decode(&bytes(0)?, &from)?;
                }
                return Ok(bytea::to_binary(bytes(0)?));
            }
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
        "json_to_tsvector" | "jsonb_to_tsvector" => crate::fts::json_filter_tsvector(args),
        "ts_headline" => {
            let at = usize::from(!matches!(
                types.first().map(String::as_str),
                Some("json" | "jsonb")
            ));
            crate::fts::json_headline(args, at, types.get(at).is_some_and(|t| t == "jsonb"))
        }
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
}

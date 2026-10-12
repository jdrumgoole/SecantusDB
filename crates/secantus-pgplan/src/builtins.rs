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
use crate::builtins_text::{decode, encode, encoding, invalid_utf8, parse_ident, Encoding};
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

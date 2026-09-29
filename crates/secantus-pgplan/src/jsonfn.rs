//! SQL values as JSON, and the JSON constructors: `to_json` / `to_jsonb`,
//! `json[b]_build_object` / `json[b]_build_array`, `json_object`,
//! `array_to_json`, `row_to_json`, `json[b]_strip_nulls`.
//!
//! `json` output is TEXT with PostgreSQL's per-function spacing, and each
//! function has its own: `to_json` / `row_to_json` / `array_to_json` are
//! compact (`{"a":1}`, `[1,2]`), `json_build_object` puts spaces round the
//! colon (`{"a" : 1, "b" : 2}`), `json_build_array` after the comma
//! (`[1, 2]`). A `jsonb` result is the canonical jsonb text. All measured
//! against PostgreSQL 14.

use bson::Bson;

use crate::json::{self, Json};
use crate::{Error, Result};

/// A SQL value of static type `ty` as JSON (`datum_to_json`'s mapping).
pub fn to_json_value(v: &Bson, ty: &str) -> Json {
    if *v == Bson::Null {
        return Json::Null;
    }
    if let Some(fields) = crate::record_fields(v) {
        let names = crate::record_names(v);
        let types = crate::record_field_types(v).unwrap_or_default();
        return Json::Object(
            fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let name = names
                        .as_ref()
                        .and_then(|n| n.get(i).cloned())
                        .unwrap_or_else(|| format!("f{}", i + 1));
                    let t = types.get(i).map_or("", String::as_str);
                    (name, to_json_value(f, t))
                })
                .collect(),
        );
    }
    let ty = ty.trim();
    if let Bson::Array(items) = v {
        let elem = ty.strip_suffix("[]").unwrap_or("");
        return Json::Array(items.iter().map(|x| to_json_value(x, elem)).collect());
    }
    match ty {
        "json" | "jsonb" => {
            let t = crate::value_text(v);
            json::parse(&t).unwrap_or(Json::Str(t))
        }
        "bool" | "boolean" => match v {
            Bson::Boolean(b) => Json::Bool(*b),
            other => Json::Str(crate::value_text(other)),
        },
        "timestamp" | "timestamp without time zone" | "timestamptz" | "timestamp with time zone" => {
            match crate::instant_micros(v) {
                Some(m) => {
                    let text = crate::render_timestamp(m).replacen(' ', "T", 1);
                    if ty.contains("tz") || ty.contains("with time zone") {
                        Json::Str(format!("{text}+00:00"))
                    } else {
                        Json::Str(text)
                    }
                }
                None => Json::Str(crate::value_text(v)),
            }
        }
        "text" | "varchar" | "bpchar" | "name" | "char" | "unknown" | "date" | "time" | "uuid" => {
            Json::Str(crate::value_text(v))
        }
        _ => match v {
            Bson::Int32(i) => Json::Number(i.to_string()),
            Bson::Int64(i) => Json::Number(i.to_string()),
            Bson::Double(d) if d.is_finite() => Json::Number(crate::geo::float8_text(*d)),
            Bson::Double(d) => Json::Str(crate::geo::float8_text(*d)),
            Bson::Boolean(b) => Json::Bool(*b),
            Bson::Decimal128(_) => Json::Number(crate::numeric::numeric_text(v).unwrap_or_default()),
            Bson::Document(d) if d.contains_key(crate::WIDE_NUMERIC_KEY) => {
                Json::Number(crate::numeric::numeric_text(v).unwrap_or_default())
            }
            other => Json::Str(
                crate::interval_value_text(other).unwrap_or_else(|| crate::value_text(other)),
            ),
        },
    }
}

/// Compact json text (`to_json`, `row_to_json`, `array_to_json`): no
/// spaces anywhere, keys in their original order.
pub fn compact(j: &Json) -> String {
    match j {
        Json::Array(items) => format!("[{}]", items.iter().map(compact).collect::<Vec<_>>().join(",")),
        Json::Object(members) => format!(
            "{{{}}}",
            members
                .iter()
                .map(|(k, v)| format!("{}:{}", json::render_jsonb(&Json::Str(k.clone())), compact(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => json::render_jsonb(other),
    }
}

/// A value inside a json result: a `json` value verbatim, everything else
/// as `to_json` renders it.
pub fn datum_json_text(v: &Bson, ty: &str) -> String {
    if *v == Bson::Null {
        return "null".into();
    }
    if ty == "json" && crate::record_fields(v).is_none() {
        return crate::value_text(v);
    }
    compact(&to_json_value(v, ty))
}

fn strip_nulls(j: Json) -> Json {
    match j {
        Json::Object(m) => Json::Object(
            m.into_iter()
                .filter(|(_, v)| *v != Json::Null)
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Json::Array(a) => Json::Array(a.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

pub const FUNCTIONS: &[&str] = &[
    "to_json",
    "to_jsonb",
    "json_build_object",
    "jsonb_build_object",
    "json_build_array",
    "jsonb_build_array",
    "json_object",
    "jsonb_object",
    "array_to_json",
    "row_to_json",
    "json_strip_nulls",
    "jsonb_strip_nulls",
];

/// A JSON constructor's result type.
pub fn result_type(name: &str) -> Option<&'static str> {
    FUNCTIONS
        .contains(&name)
        .then(|| if name.starts_with("jsonb") || name == "to_jsonb" { "jsonb" } else { "json" })
}

fn text_of_key(v: &Bson) -> Result<String> {
    if *v == Bson::Null {
        return Err(Error::Sqlstate(
            "22004",
            "null value not allowed for object key".into(),
        ));
    }
    Ok(match v {
        Bson::Boolean(b) => (if *b { "true" } else { "false" }).to_string(),
        other => crate::value_text(other),
    })
}

/// Evaluate a JSON constructor over its arguments and their static types.
pub fn call(name: &str, args: &[Bson], types: &[String]) -> Result<Bson> {
    let t = |i: usize| types.get(i).map_or("", String::as_str);
    let jsonb = name.starts_with("jsonb") || name == "to_jsonb";
    let out = |j: Json| -> Bson {
        Bson::String(if jsonb { json::render_jsonb(&j) } else { compact(&j) })
    };
    match name {
        "to_json" | "to_jsonb" => {
            let [v] = args else { return Err(wrong(name)) };
            if *v == Bson::Null {
                return Ok(Bson::Null);
            }
            if jsonb {
                Ok(out(to_json_value(v, t(0))))
            } else {
                Ok(Bson::String(datum_json_text(v, t(0))))
            }
        }
        "row_to_json" | "array_to_json" => {
            let Some(v) = args.first() else { return Err(wrong(name)) };
            if *v == Bson::Null {
                return Ok(Bson::Null);
            }
            if name == "array_to_json" && !matches!(v, Bson::Array(_)) {
                return Err(Error::Sqlstate("22023", "argument must be an array".into()));
            }
            Ok(Bson::String(compact(&to_json_value(v, t(0)))))
        }
        "json_build_object" | "jsonb_build_object" => {
            if args.len() % 2 != 0 {
                return Err(Error::Sqlstate(
                    "22023",
                    "argument list must have even number of elements".into(),
                ));
            }
            for i in (0..args.len()).step_by(2) {
                if args[i] == Bson::Null {
                    return Err(Error::Sqlstate(
                        "22023",
                        format!("argument {} cannot be null", i + 1),
                    ));
                }
            }
            if jsonb {
                let mut members = Vec::new();
                for i in (0..args.len()).step_by(2) {
                    members.push((text_of_key(&args[i])?, to_json_value(&args[i + 1], t(i + 1))));
                }
                return Ok(out(Json::Object(members)));
            }
            let mut parts = Vec::new();
            for i in (0..args.len()).step_by(2) {
                let k = text_of_key(&args[i])?;
                parts.push(format!(
                    "{} : {}",
                    json::render_jsonb(&Json::Str(k)),
                    datum_json_text(&args[i + 1], t(i + 1))
                ));
            }
            Ok(Bson::String(format!("{{{}}}", parts.join(", "))))
        }
        "json_build_array" | "jsonb_build_array" => {
            if jsonb {
                return Ok(out(Json::Array(
                    args.iter().enumerate().map(|(i, a)| to_json_value(a, t(i))).collect(),
                )));
            }
            let parts: Vec<String> = args.iter().enumerate().map(|(i, a)| datum_json_text(a, t(i))).collect();
            Ok(Bson::String(format!("[{}]", parts.join(", "))))
        }
        "json_object" | "jsonb_object" => {
            let strings = |v: &Bson| -> Result<Vec<Option<String>>> {
                match v {
                    Bson::Array(items) => Ok(items
                        .iter()
                        .map(|x| if *x == Bson::Null { None } else { Some(crate::value_text(x)) })
                        .collect()),
                    Bson::String(s) => match crate::parse_array(s, "text")? {
                        Bson::Array(items) => Ok(items
                            .iter()
                            .map(|x| if *x == Bson::Null { None } else { Some(crate::value_text(x)) })
                            .collect()),
                        _ => Ok(Vec::new()),
                    },
                    _ => Err(Error::Sqlstate("22023", "array must have even number of elements".into())),
                }
            };
            let pairs: Vec<(String, Option<String>)> = match args {
                [one] => {
                    if *one == Bson::Null {
                        return Ok(Bson::Null);
                    }
                    let items = strings(one)?;
                    if items.len() % 2 != 0 {
                        return Err(Error::Sqlstate("2202E", "array must have even number of elements".into()));
                    }
                    let mut out = Vec::new();
                    for c in items.chunks(2) {
                        let k = c[0].clone().ok_or_else(|| {
                            Error::Sqlstate("22004", "null value not allowed for object key".into())
                        })?;
                        out.push((k, c[1].clone()));
                    }
                    out
                }
                [ks, vs] => {
                    if *ks == Bson::Null || *vs == Bson::Null {
                        return Ok(Bson::Null);
                    }
                    let (k, v) = (strings(ks)?, strings(vs)?);
                    if k.len() != v.len() {
                        return Err(Error::Sqlstate("2202E", "mismatched array dimensions".into()));
                    }
                    let mut out = Vec::new();
                    for (k, v) in k.into_iter().zip(v) {
                        let k = k.ok_or_else(|| {
                            Error::Sqlstate("22004", "null value not allowed for object key".into())
                        })?;
                        out.push((k, v));
                    }
                    out
                }
                _ => return Err(wrong(name)),
            };
            let as_json = |v: &Option<String>| v.clone().map_or(Json::Null, Json::Str);
            if jsonb {
                return Ok(out(Json::Object(pairs.iter().map(|(k, v)| (k.clone(), as_json(v))).collect())));
            }
            let parts: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{} : {}", json::render_jsonb(&Json::Str(k.clone())), json::render_jsonb(&as_json(v))))
                .collect();
            Ok(Bson::String(format!("{{{}}}", parts.join(", "))))
        }
        "json_strip_nulls" | "jsonb_strip_nulls" => {
            let [v] = args else { return Err(wrong(name)) };
            if *v == Bson::Null {
                return Ok(Bson::Null);
            }
            let text = crate::value_text(v);
            let j = json::parse(&text).map_err(|_| {
                Error::InvalidText(format!("invalid input syntax for type json: \"{text}\""))
            })?;
            Ok(out(strip_nulls(j)))
        }
        _ => Err(wrong(name)),
    }
}

fn wrong(name: &str) -> Error {
    Error::UndefinedFunction(format!("function {name} does not exist"))
}

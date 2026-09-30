//! Changing and inspecting JSON: `jsonb_set` / `jsonb_set_lax` /
//! `jsonb_insert`, the `-` / `#-` / `||` operators, `jsonb_pretty`,
//! `json[b]_typeof`, `json[b]_array_length` and `json[b]_extract_path[_text]`.
//!
//! Semantics are PostgreSQL 14's (`jsonfuncs.c`): an array step is an
//! integer (22P02 otherwise), negative counts from the end, and a step past
//! either end appends / prepends when creating and changes nothing when not;
//! a missing intermediate step changes nothing; a scalar target cannot be
//! walked into (22023).

use bson::Bson;

use crate::json::{self, Json};
use crate::{Error, Result};

fn parse(v: &Bson) -> Result<Json> {
    let text = crate::value_text(v);
    json::parse(&text)
        .map_err(|_| Error::InvalidText(format!("invalid input syntax for type json: \"{text}\"")))
}

fn out(j: &Json) -> Bson {
    Bson::String(json::render_jsonb(j))
}

fn path_of(v: &Bson) -> Result<Vec<String>> {
    Ok(match v {
        Bson::Array(items) => items.iter().map(crate::value_text).collect(),
        other => match crate::cast_value(other.clone(), "text[]")? {
            Bson::Array(items) => items.iter().map(crate::value_text).collect(),
            _ => Vec::new(),
        },
    })
}

/// An array step: an integer, else 22P02 naming its 1-based position.
fn index_step(step: &str, position: usize) -> Result<i64> {
    step.trim().parse::<i64>().map_err(|_| {
        Error::Sqlstate(
            "22P02",
            format!("path element at position {position} is not an integer: \"{step}\""),
        )
    })
}

/// Where an index lands in an array of `len`: `Ok(i)` inside, `Err(false)`
/// before the start, `Err(true)` past the end.
fn resolve(idx: i64, len: usize) -> std::result::Result<usize, bool> {
    let len = len as i64;
    let i = if idx < 0 { len + idx } else { idx };
    if i < 0 {
        Err(false)
    } else if i >= len {
        Err(true)
    } else {
        Ok(i as usize)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// `jsonb_set`: replace, or create at the last step when asked.
    Set { create: bool },
    /// `jsonb_insert`: add into an array before / after, or a new key.
    Insert { after: bool },
    /// `#-`: remove.
    Delete,
}

fn walk(target: &mut Json, path: &[String], depth: usize, value: &Json, mode: Mode) -> Result<()> {
    let last = depth + 1 == path.len();
    let step = &path[depth];
    match target {
        Json::Object(members) => {
            let at = members.iter().position(|(k, _)| k == step);
            if last {
                match (mode, at) {
                    (Mode::Delete, Some(i)) => {
                        members.remove(i);
                    }
                    (Mode::Delete, None) => {}
                    (Mode::Set { .. }, Some(i)) => members[i].1 = value.clone(),
                    (Mode::Set { create: true }, None) | (Mode::Insert { .. }, None) => {
                        members.push((step.clone(), value.clone()));
                    }
                    (Mode::Set { create: false }, None) => {}
                    (Mode::Insert { .. }, Some(_)) => {
                        return Err(Error::Sqlstate(
                            "22023",
                            "cannot replace existing key".into(),
                        ))
                    }
                }
                return Ok(());
            }
            if let Some(i) = at {
                walk(&mut members[i].1, path, depth + 1, value, mode)?;
            }
            Ok(())
        }
        Json::Array(items) => {
            let idx = index_step(step, depth + 1)?;
            let at = resolve(idx, items.len());
            if last {
                match (mode, at) {
                    (Mode::Delete, Ok(i)) => {
                        items.remove(i);
                    }
                    (Mode::Delete, Err(_)) => {}
                    (Mode::Set { .. }, Ok(i)) => items[i] = value.clone(),
                    (Mode::Set { create: true }, Err(past_end)) => {
                        if past_end {
                            items.push(value.clone());
                        } else {
                            items.insert(0, value.clone());
                        }
                    }
                    (Mode::Set { create: false }, Err(_)) => {}
                    (Mode::Insert { after }, Ok(i)) => {
                        items.insert(if after { i + 1 } else { i }, value.clone());
                    }
                    (Mode::Insert { .. }, Err(past_end)) => {
                        if past_end {
                            items.push(value.clone());
                        } else {
                            items.insert(0, value.clone());
                        }
                    }
                }
                return Ok(());
            }
            if let Ok(i) = at {
                walk(&mut items[i], path, depth + 1, value, mode)?;
            }
            Ok(())
        }
        _ => Err(Error::Sqlstate(
            "22023",
            match mode {
                Mode::Delete => "cannot delete path in scalar",
                _ => "cannot set path in scalar",
            }
            .into(),
        )),
    }
}

fn modify(target: &Bson, path: &Bson, value: &Json, mode: Mode) -> Result<Bson> {
    let mut j = parse(target)?;
    let path = path_of(path)?;
    if path.is_empty() {
        return Ok(out(&j));
    }
    if !matches!(j, Json::Object(_) | Json::Array(_)) {
        return Err(Error::Sqlstate(
            "22023",
            match mode {
                Mode::Delete => "cannot delete path in scalar",
                _ => "cannot set path in scalar",
            }
            .into(),
        ));
    }
    walk(&mut j, &path, 0, value, mode)?;
    Ok(out(&j))
}

/// `jsonb - text`, `jsonb - int`, `jsonb - text[]`; the right operand's
/// type decides which, since a bare integer is an index and anything else a
/// key.
pub fn minus(lhs: &Bson, rhs: &Bson, rhs_type: &str) -> Result<Bson> {
    let mut j = parse(lhs)?;
    let int = matches!(rhs, Bson::Int32(_) | Bson::Int64(_)) && !rhs_type.ends_with("[]");
    if int {
        let idx = match rhs {
            Bson::Int32(i) => i64::from(*i),
            Bson::Int64(i) => *i,
            _ => 0,
        };
        return match &mut j {
            Json::Array(items) => {
                if let Ok(i) = resolve(idx, items.len()) {
                    items.remove(i);
                }
                Ok(out(&j))
            }
            Json::Object(_) => Err(Error::Sqlstate(
                "22023",
                "cannot delete from object using integer index".into(),
            )),
            _ => Err(Error::Sqlstate("22023", "cannot delete from scalar".into())),
        };
    }
    let keys: Vec<String> = match rhs {
        Bson::Array(items) => items.iter().map(crate::value_text).collect(),
        other if rhs_type == "text[]" => path_of(other)?,
        other => vec![crate::value_text(other)],
    };
    match &mut j {
        Json::Object(members) => members.retain(|(k, _)| !keys.contains(k)),
        Json::Array(items) => {
            items.retain(|x| !matches!(x, Json::Str(s) if keys.contains(s)));
        }
        _ => return Err(Error::Sqlstate("22023", "cannot delete from scalar".into())),
    }
    Ok(out(&j))
}

/// `jsonb #- text[]`.
pub fn delete_path(lhs: &Bson, path: &Bson) -> Result<Bson> {
    modify(lhs, path, &Json::Null, Mode::Delete)
}

/// `jsonb || jsonb`: objects merge (the right's keys win), arrays
/// concatenate, and anything else is wrapped into an array first.
pub fn concat(lhs: &Bson, rhs: &Bson) -> Result<Bson> {
    let (l, r) = (parse(lhs)?, parse(rhs)?);
    let joined = match (l, r) {
        (Json::Object(mut a), Json::Object(b)) => {
            for (k, v) in b {
                a.retain(|(x, _)| *x != k);
                a.push((k, v));
            }
            Json::Object(a)
        }
        (Json::Array(mut a), Json::Array(b)) => {
            a.extend(b);
            Json::Array(a)
        }
        (Json::Array(mut a), other) => {
            a.push(other);
            Json::Array(a)
        }
        (other, Json::Array(b)) => {
            let mut a = vec![other];
            a.extend(b);
            Json::Array(a)
        }
        (a, b) => Json::Array(vec![a, b]),
    };
    Ok(out(&joined))
}

/// `jsonb_pretty`: four-space indentation, one member per line, and an
/// empty container as its brackets on two lines.
pub fn pretty(j: &Json) -> String {
    fn go(j: &Json, level: usize, s: &mut String) {
        let pad = |n: usize| "    ".repeat(n);
        match j {
            Json::Object(members) => {
                s.push_str("{\n");
                for (i, (k, v)) in members.iter().enumerate() {
                    s.push_str(&pad(level + 1));
                    s.push_str(&json::render_jsonb(&Json::Str(k.clone())));
                    s.push_str(": ");
                    go(v, level + 1, s);
                    if i + 1 < members.len() {
                        s.push(',');
                    }
                    s.push('\n');
                }
                s.push_str(&pad(level));
                s.push('}');
            }
            Json::Array(items) => {
                s.push_str("[\n");
                for (i, v) in items.iter().enumerate() {
                    s.push_str(&pad(level + 1));
                    go(v, level + 1, s);
                    if i + 1 < items.len() {
                        s.push(',');
                    }
                    s.push('\n');
                }
                s.push_str(&pad(level));
                s.push(']');
            }
            scalar => s.push_str(&json::render_jsonb(scalar)),
        }
    }
    // Normalised first: jsonb's key order and duplicate rule.
    let normal = json::parse(&json::render_jsonb(j)).unwrap_or_else(|_| j.clone());
    let mut s = String::new();
    go(&normal, 0, &mut s);
    s
}

pub const FUNCTIONS: &[&str] = &[
    "jsonb_set",
    "jsonb_set_lax",
    "jsonb_insert",
    "jsonb_pretty",
    "jsonb_typeof",
    "json_typeof",
    "jsonb_array_length",
    "json_array_length",
    "jsonb_extract_path",
    "json_extract_path",
    "jsonb_extract_path_text",
    "json_extract_path_text",
];

pub fn result_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "jsonb_set" | "jsonb_set_lax" | "jsonb_insert" | "jsonb_extract_path" => "jsonb",
        "json_extract_path" => "json",
        "jsonb_pretty"
        | "jsonb_typeof"
        | "json_typeof"
        | "jsonb_extract_path_text"
        | "json_extract_path_text" => "text",
        "jsonb_array_length" | "json_array_length" => "int4",
        _ => return None,
    })
}

/// Evaluate one of `FUNCTIONS`; `None` when `name` is not one.
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !FUNCTIONS.contains(&name) {
        return None;
    }
    Some(eval(name, args))
}

fn eval(name: &str, args: &[Bson]) -> Result<Bson> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Bson::Null);
    let bool_arg = |i: usize, default: bool| match args.get(i) {
        None => default,
        Some(Bson::Boolean(b)) => *b,
        Some(other) => matches!(crate::value_text(other).as_str(), "t" | "true"),
    };
    // `jsonb_set_lax` alone gives a NULL new value a meaning.
    if name != "jsonb_set_lax" && args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    match name {
        "jsonb_set" => modify(
            &arg(0),
            &arg(1),
            &parse(&arg(2))?,
            Mode::Set {
                create: bool_arg(3, true),
            },
        ),
        "jsonb_set_lax" => {
            if arg(0) == Bson::Null || arg(1) == Bson::Null {
                return Ok(Bson::Null);
            }
            if arg(2) != Bson::Null {
                return modify(
                    &arg(0),
                    &arg(1),
                    &parse(&arg(2))?,
                    Mode::Set {
                        create: bool_arg(3, true),
                    },
                );
            }
            let treatment = match args.get(4) {
                Some(Bson::Null) => {
                    return Err(Error::Sqlstate(
                        "22004",
                        "null_value_treatment must be \"delete_key\", \"return_target\", \"use_json_null\", or \"raise_exception\"".into(),
                    ))
                }
                Some(v) => crate::value_text(v),
                None => "use_json_null".to_string(),
            };
            match treatment.as_str() {
                "raise_exception" => Err(Error::Sqlstate(
                    "22004",
                    "JSON value must not be null".into(),
                )),
                "use_json_null" => modify(&arg(0), &arg(1), &Json::Null, Mode::Set {
                    create: bool_arg(3, true),
                }),
                "delete_key" => delete_path(&arg(0), &arg(1)),
                "return_target" => Ok(out(&parse(&arg(0))?)),
                _ => Err(Error::Sqlstate(
                    "22023",
                    "null_value_treatment must be \"delete_key\", \"return_target\", \"use_json_null\", or \"raise_exception\"".into(),
                )),
            }
        }
        "jsonb_insert" => modify(
            &arg(0),
            &arg(1),
            &parse(&arg(2))?,
            Mode::Insert {
                after: bool_arg(3, false),
            },
        ),
        "jsonb_pretty" => Ok(Bson::String(pretty(&parse(&arg(0))?))),
        "jsonb_typeof" | "json_typeof" => Ok(Bson::String(
            match parse(&arg(0))? {
                Json::Object(_) => "object",
                Json::Array(_) => "array",
                Json::Str(_) => "string",
                Json::Number(_) => "number",
                Json::Bool(_) => "boolean",
                Json::Null => "null",
            }
            .into(),
        )),
        "jsonb_array_length" | "json_array_length" => match parse(&arg(0))? {
            Json::Array(items) => Ok(Bson::Int32(items.len() as i32)),
            Json::Object(_) => Err(Error::Sqlstate(
                "22023",
                "cannot get array length of a non-array".into(),
            )),
            _ => Err(Error::Sqlstate(
                "22023",
                "cannot get array length of a scalar".into(),
            )),
        },
        _ => {
            // The extract_path family: the rest of the arguments are the path.
            let j = parse(&arg(0))?;
            let mut current = Some(&j);
            for step in &args[1..] {
                let step = crate::value_text(step);
                current = current.and_then(|v| json::member(v, &step));
            }
            let Some(found) = current else {
                return Ok(Bson::Null);
            };
            Ok(if name.ends_with("_text") {
                json::as_sql_text(found).map_or(Bson::Null, Bson::String)
            } else if name.starts_with("jsonb") {
                out(found)
            } else {
                Bson::String(json::render_json(found))
            })
        }
    }
}

//! jsonb subscripting: `j['a'][1]`, `SET j['a'] = v`, `x['a'] := v`
//! (RP-772).
//!
//! A port of PostgreSQL 15's `jsonbsubs.c` and the two `jsonfuncs.c`
//! routines it calls:
//!
//! - `jsonb_subscript_transform`: every subscript is coerced to `int4` or
//!   to `text`, whichever ONE it coerces to implicitly (an untyped literal is
//!   `text`); a slice is 42804.
//! - `jsonb_subscript_check_subscripts`: an `int4` subscript becomes its
//!   text (`int4out`), so the path is text throughout; a NULL subscript
//!   makes a fetch NULL and an assignment 22004.
//! - a fetch is `jsonb_get_element` (`#>`'s walk): a key over an object, an
//!   index (`strtoint`, negative from the end) over an array, NULL for
//!   anything missing or a step into a scalar.
//! - an assignment is `jsonb_set_element`, `setPath` with `JB_PATH_CREATE |
//!   JB_PATH_FILL_GAPS | JB_PATH_CONSISTENT_POSITION`: a missing key is
//!   added, a missing path is built (an object for a text step, an array
//!   for an integer one), an array is padded with nulls up to the index, a
//!   negative index past the start is 22023, and a step into a scalar is
//!   22023 `cannot replace existing key`. A NULL source is an empty array
//!   when the first subscript is an `int4`, an empty object otherwise.

use bson::Bson;

use crate::json::{self, Json};
use crate::{Error, Result};

/// The type a jsonb subscript is coerced to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptKind {
    Int,
    Text,
}

/// 42804 for any slice in the chain (`jsonb_subscript_transform`).
pub fn slice_error() -> Error {
    Error::DatatypeMismatch("jsonb subscript does not support slices".into())
}

/// The type a subscript of type `source` is coerced to, as
/// `jsonb_subscript_transform` picks it: `unknown` (an untyped literal) is
/// `text`; otherwise the one of `int4` and `text` it coerces to implicitly.
/// No built-in type coerces implicitly to both, so the transform's "only
/// one type" refusal is not reachable here.
pub fn subscript_kind(source: &str) -> Result<SubscriptKind> {
    let source = source.trim();
    if source.is_empty() || source == "unknown" {
        return Ok(SubscriptKind::Text);
    }
    if crate::implicit_pathway(source, "int4") {
        return Ok(SubscriptKind::Int);
    }
    if crate::implicit_pathway(source, "text") {
        return Ok(SubscriptKind::Text);
    }
    Err(Error::DatatypeMismatch(format!(
        "subscript type {} is not supported",
        crate::display_type(source)
    )))
}

/// One path step as `jsonb_subscript_check_subscripts` leaves it: the
/// text of the subscript coerced to its kind, `None` for NULL.
pub fn path_step(value: &Bson, kind: SubscriptKind) -> Result<Option<String>> {
    if matches!(value, Bson::Null | Bson::Undefined) {
        return Ok(None);
    }
    Ok(Some(match kind {
        SubscriptKind::Int => match crate::cast_value(value.clone(), "int4")? {
            Bson::Int32(i) => i.to_string(),
            Bson::Null => return Ok(None),
            other => crate::value_text(&other),
        },
        SubscriptKind::Text => crate::value_text(value),
    }))
}

/// 22004 for a NULL subscript of an assignment.
pub fn null_subscript_error() -> Error {
    Error::Sqlstate(
        "22004",
        "jsonb subscript in assignment must not be null".into(),
    )
}

/// C's `strtoint(s, &end, 10)` with the callers' checks (`end == s`, `*end
/// != '\0'`, `errno`): leading white space and a sign are taken, anything
/// after the digits or a value outside `int4` is not an integer.
fn strtoint(s: &str) -> Option<i32> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for b in digits.bytes() {
        v = v * 10 + i64::from(b - b'0');
        if v > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    let v = if neg { -v } else { v };
    i32::try_from(v).ok()
}

fn parse(v: &Bson) -> Result<Json> {
    json::parse(&crate::value_text(v))
        .map_err(|_| Error::InvalidText("invalid input syntax for type json".to_string()))
}

/// `jsonb_get_element(jb, path, npath, isnull, as_text = false)`.
pub fn fetch(source: &Bson, path: &[String]) -> Result<Bson> {
    if matches!(source, Bson::Null) {
        return Ok(Bson::Null);
    }
    let root = parse(source)?;
    let mut current = &root;
    for step in path {
        let next = match current {
            Json::Object(members) => members.iter().find(|(k, _)| k == step).map(|(_, v)| v),
            Json::Array(items) => {
                let Some(index) = strtoint(step) else {
                    return Ok(Bson::Null);
                };
                let len = items.len() as i64;
                let index = i64::from(index);
                let at = if index >= 0 {
                    index
                } else if -index > len {
                    return Ok(Bson::Null);
                } else {
                    len + index
                };
                items.get(at as usize)
            }
            // A scalar: extraction yields a null.
            _ => return Ok(Bson::Null),
        };
        match next {
            Some(v) => current = v,
            None => return Ok(Bson::Null),
        }
    }
    Ok(Bson::String(json::render_jsonb(current)))
}

/// `jsonb_subscript_assign`: `newval` (NULL is JSON `null`) placed at
/// `path` in `source`, a NULL source starting as `[]` when `expect_array`
/// (the first subscript is an `int4`) and `{}` otherwise.
pub fn assign(source: &Bson, path: &[String], expect_array: bool, newval: &Bson) -> Result<Bson> {
    let mut root = if matches!(source, Bson::Null) {
        if expect_array {
            Json::Array(Vec::new())
        } else {
            Json::Object(Vec::new())
        }
    } else {
        parse(source)?
    };
    let value = if matches!(newval, Bson::Null) {
        Json::Null
    } else {
        parse(newval)?
    };
    if path.is_empty() {
        return Ok(Bson::String(json::render_jsonb(&root)));
    }
    set_path(&mut root, path, 0, value)?;
    Ok(Bson::String(json::render_jsonb(&root)))
}

fn cannot_replace() -> Error {
    Error::Sqlstate(
        "22023",
        "cannot replace existing key\nDetail: The path assumes key is a composite object, \
         but it is a scalar value."
            .into(),
    )
}

/// `setPath` at `level`, over the value found there. With
/// `JB_PATH_FILL_GAPS` a scalar is refused whenever a path step remains to
/// be taken in it, a raw scalar root included.
fn set_path(target: &mut Json, path: &[String], level: usize, newval: Json) -> Result<()> {
    match target {
        Json::Object(members) => set_path_object(members, path, level, newval),
        Json::Array(items) => set_path_array(items, path, level, newval),
        _ => Err(cannot_replace()),
    }
}

/// `setPathObject`: the key replaced in place, or added (`JB_PATH_CREATE`)
/// when it is the last step, or built down to the value (`push_path`) when
/// it is not.
fn set_path_object(
    members: &mut Vec<(String, Json)>,
    path: &[String],
    level: usize,
    newval: Json,
) -> Result<()> {
    let key = &path[level];
    let last = level + 1 == path.len();
    match members.iter().position(|(k, _)| k == key) {
        Some(i) if last => members[i].1 = newval,
        Some(i) => set_path(&mut members[i].1, path, level + 1, newval)?,
        None if last => members.push((key.clone(), newval)),
        None => members.push((key.clone(), push_path(path, level, newval))),
    }
    Ok(())
}

/// `setPathArray` under `JB_PATH_CONSISTENT_POSITION | JB_PATH_FILL_GAPS`.
fn set_path_array(
    items: &mut Vec<Json>,
    path: &[String],
    level: usize,
    newval: Json,
) -> Result<()> {
    let step = &path[level];
    let Some(mut idx) = strtoint(step) else {
        return Err(Error::Sqlstate(
            "22P02",
            format!(
                "path element at position {} is not an integer: \"{step}\"",
                level + 1
            ),
        ));
    };
    let nelems = items.len() as i64;
    if idx < 0 {
        if -i64::from(idx) > nelems {
            return Err(Error::Sqlstate(
                "22023",
                format!(
                    "path element at position {} is out of range: {idx}",
                    level + 1
                ),
            ));
        }
        idx = (nelems + i64::from(idx)) as i32;
    }
    let last = level + 1 == path.len();
    let at = i64::from(idx);
    if at < nelems {
        let slot = &mut items[at as usize];
        if last {
            *slot = newval;
        } else {
            set_path(slot, path, level + 1, newval)?;
        }
        return Ok(());
    }
    // Past the end: nulls up to the index, then the value (or the path
    // built down to it).
    items.extend(std::iter::repeat_n(Json::Null, (at - nelems) as usize));
    items.push(if last {
        newval
    } else {
        push_path(path, level, newval)
    });
    Ok(())
}

/// `push_path`: the containers for the steps after `level`, each an object
/// for a text step and an array (padded with nulls to the index) for an
/// integer one, with `newval` at the bottom.
fn push_path(path: &[String], level: usize, newval: Json) -> Json {
    let mut value = newval;
    for step in path[level + 1..].iter().rev() {
        value = match strtoint(step) {
            Some(index) => {
                let mut items: Vec<Json> =
                    std::iter::repeat_n(Json::Null, index.max(0) as usize).collect();
                items.push(value);
                Json::Array(items)
            }
            None => Json::Object(vec![(step.clone(), value)]),
        };
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Bson {
        Bson::String(v.into())
    }

    fn p(steps: &[&str]) -> Vec<String> {
        steps.iter().map(|x| x.to_string()).collect()
    }

    fn text(b: Bson) -> String {
        crate::value_text(&b)
    }

    // Each expected answer was measured on PostgreSQL 15.19.
    #[test]
    fn fetch_walks_like_jsonb_get_element() {
        let j = s(r#"{"a": [1, 2]}"#);
        assert_eq!(text(fetch(&j, &p(&["a", "1"])).unwrap()), "2");
        assert_eq!(text(fetch(&j, &p(&["a", "-1"])).unwrap()), "2");
        assert_eq!(fetch(&j, &p(&["a", "-3"])).unwrap(), Bson::Null);
        assert_eq!(fetch(&s("[1, 2]"), &p(&[" 1"])).unwrap(), s("2"));
        assert_eq!(fetch(&s("[1, 2]"), &p(&["+1"])).unwrap(), s("2"));
        assert_eq!(fetch(&s("[1, 2]"), &p(&["1 "])).unwrap(), Bson::Null);
        assert_eq!(fetch(&s(r#"{"1": 5}"#), &p(&["1"])).unwrap(), s("5"));
        assert_eq!(
            fetch(&s(r#"{"a": 1}"#), &p(&["a", "b"])).unwrap(),
            Bson::Null
        );
        assert_eq!(fetch(&s(r#""x""#), &p(&["0"])).unwrap(), Bson::Null);
    }

    /// A source (`None` is SQL NULL), a path, whether the first subscript is
    /// an integer, the value, and PostgreSQL's answer.
    type AssignCase = (
        Option<&'static str>,
        &'static [&'static str],
        bool,
        &'static str,
        &'static str,
    );

    #[test]
    fn assign_creates_and_fills_like_jsonb_set_element() {
        let cases: &[AssignCase] = &[
            (
                Some(r#"{"a": [1]}"#),
                &["b"],
                false,
                "2",
                r#"{"a": [1], "b": 2}"#,
            ),
            (
                Some(r#"{"a": [1]}"#),
                &["a", "3"],
                false,
                "5",
                r#"{"a": [1, null, null, 5]}"#,
            ),
            (
                Some("[1]"),
                &["5"],
                true,
                "2",
                "[1, null, null, null, null, 2]",
            ),
            (Some("[1, 2]"), &["-1"], true, "9", "[1, 9]"),
            (None, &["2"], true, "1", "[null, null, 1]"),
            (None, &["a", "b"], false, "1", r#"{"a": {"b": 1}}"#),
            (
                Some("{}"),
                &["a", "2", "b"],
                false,
                "1",
                r#"{"a": [null, null, {"b": 1}]}"#,
            ),
            (
                Some("[]"),
                &["1", "a", "0"],
                true,
                "1",
                r#"[null, {"a": [1]}]"#,
            ),
            (
                Some("[0]"),
                &["3", "a"],
                true,
                "1",
                r#"[0, null, null, {"a": 1}]"#,
            ),
            (
                Some(r#"{"z": 1}"#),
                &["a", "b"],
                false,
                "1",
                r#"{"a": {"b": 1}, "z": 1}"#,
            ),
            (
                Some(r#"{"a": 1}"#),
                &["1"],
                true,
                "5",
                r#"{"1": 5, "a": 1}"#,
            ),
            (Some("[1, [2]]"), &["1", "0"], true, "5", "[1, [5]]"),
            (
                Some(r#"{"a": [1]}"#),
                &["a", "2", "b"],
                false,
                "5",
                r#"{"a": [1, null, {"b": 5}]}"#,
            ),
        ];
        for (source, path, expect_array, value, want) in cases {
            let source = source.map_or(Bson::Null, s);
            let got = assign(&source, &p(path), *expect_array, &s(value)).unwrap();
            assert_eq!(text(got), *want, "{source:?} {path:?}");
        }
        assert_eq!(
            text(assign(&s(r#"{"a": 1}"#), &p(&["a"]), false, &Bson::Null).unwrap()),
            r#"{"a": null}"#
        );
    }

    const REPLACE: &str = "cannot replace existing key\nDetail: The path assumes key is a \
                           composite object, but it is a scalar value.";

    #[test]
    fn assign_refuses_like_set_path() {
        let err = |source: Option<&str>, path: &[&str], expect_array: bool| {
            let source = source.map_or(Bson::Null, s);
            let e = assign(&source, &p(path), expect_array, &s("1")).unwrap_err();
            (e.sqlstate(), e.to_string())
        };
        assert_eq!(
            err(Some("[1]"), &["-5"], true),
            (
                "22023",
                "path element at position 1 is out of range: -5".into()
            )
        );
        assert_eq!(
            err(None, &["-1"], true),
            (
                "22023",
                "path element at position 1 is out of range: -1".into()
            )
        );
        assert_eq!(
            err(Some(r#"{"a": []}"#), &["a", "-1", "b"], false),
            (
                "22023",
                "path element at position 2 is out of range: -1".into()
            )
        );
        assert_eq!(err(Some("1"), &["a"], false), ("22023", REPLACE.into()));
        assert_eq!(
            err(Some(r#"{"a": 1}"#), &["a", "b"], false),
            ("22023", REPLACE.into())
        );
        assert_eq!(
            err(Some("[1]"), &["a"], false),
            (
                "22P02",
                "path element at position 1 is not an integer: \"a\"".into()
            )
        );
    }

    #[test]
    fn subscript_types_follow_jsonb_subscript_transform() {
        assert_eq!(subscript_kind("int4").unwrap(), SubscriptKind::Int);
        assert_eq!(subscript_kind("int2").unwrap(), SubscriptKind::Int);
        assert_eq!(subscript_kind("unknown").unwrap(), SubscriptKind::Text);
        for t in ["text", "varchar", "bpchar", "name"] {
            assert_eq!(subscript_kind(t).unwrap(), SubscriptKind::Text, "{t}");
        }
        for (t, shown) in [
            ("int8", "bigint"),
            ("numeric", "numeric"),
            ("bool", "boolean"),
        ] {
            assert_eq!(
                subscript_kind(t).unwrap_err().to_string(),
                format!("subscript type {shown} is not supported")
            );
        }
    }
}

//! Wrong-typed command arguments: mongod's replies, per slot.
//!
//! The Python server's sweep (#1078 / #1080 / #1084 / #1085) took it to 87/87
//! clean against mongod 6.0.16; the first sweep of THIS server, on 2026-08-29,
//! found 78 of the same 87 cases divergent — 54 of them accepted outright.
//! Reproduce with `tools/probes/arg_types_extended.py` and `PROBE_SERVER`.
//!
//! "Accepted" understated it: a wrong-typed argument does not merely return the
//! wrong status, it makes the server do the wrong thing and report success.
//! `createIndexes.indexes: 5` created NO index and answered ok:1;
//! `update.multi: {}` updated one document of two; `findAndModify.upsert: [1]`
//! skipped the upsert. A driver is told each of those worked.
//!
//! **mongod's strictness is per-slot, not per-class**, which is why this is a
//! table of individual validators rather than one rule. The counterexample that
//! makes the point: `delete.deletes.limit` is NOT type-checked by mongod —
//! `{}` / `"x"` / `[1]` / `0` all mean "no limit" — while the analogous
//! `find.limit` is a type error. A blanket "validate every numeric argument"
//! rule fixes one and breaks the other.
//!
//! Every message here mirrors the Python server's, which is pinned byte-for-byte
//! against a live mongod by `tests/test_command_arg_types.py`,
//! `tests/test_arg_types_numeric.py` and `tests/test_arg_types_accepted_slots.py`.
//! Two of them look like typos and are not: mongod really does emit
//! `Expected field filterto be of type object` (no space), and really does
//! close the numeric list with an unbalanced quote.

use bson::{Bson, Document};
use secantus_core::query::bson_type_name;

use crate::CommandError;

/// mongod's numeric-slot type list, reproduced verbatim — the unbalanced quote
/// is mongod's own.
const NUMERIC_TYPES: &str = "'[long, int, decimal, double']";

/// `findAndModify.upsert` accepts a bool OR any number, unlike the strict-bool
/// `update.updates.multi` beside it.
const BOOL_OR_NUMBER_TYPES: &str = "'[bool, long, int, decimal, double]'";

fn type_mismatch(errmsg: impl Into<String>) -> CommandError {
    CommandError::new(14, "TypeMismatch", errmsg.into())
}

fn bad_value(errmsg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", errmsg.into())
}

/// `BSON field '<path>' is the wrong type '<t>', expected type 'object'`.
///
/// An absent field and an explicit `null` are both accepted here — that is
/// mongod's behaviour for this family, and it differs from the
/// `Expected field …` family below, which rejects an explicit null.
pub fn require_object(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) | Some(Bson::Document(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'object'",
            bson_type_name(v)
        ))),
    }
}

/// `BSON field '<path>' is the wrong type '<t>', expected type 'array'`.
pub fn require_array(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) | Some(Bson::Array(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'array'",
            bson_type_name(v)
        ))),
    }
}

/// `BSON field '<path>' is the wrong type '<t>', expected type 'string'`.
pub fn require_string(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) | Some(Bson::String(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'string'",
            bson_type_name(v)
        ))),
    }
}

/// mongod's numeric-slot type error.
///
/// `bool` is rejected explicitly: it is not a number to mongod, and a language
/// that treats it as one would read `limit: true` as `limit: 1`.
pub fn require_number(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None
        | Some(Bson::Null)
        | Some(Bson::Int32(_))
        | Some(Bson::Int64(_))
        | Some(Bson::Double(_))
        | Some(Bson::Decimal128(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected types {NUMERIC_TYPES}",
            bson_type_name(v)
        ))),
    }
}

/// The `find`-family wording. Note the missing space in `filterto`: mongod's
/// own message, and fidelity means reproducing it.
///
/// Unlike `require_object`, an explicit `null` IS rejected here — absent is
/// fine, `{find: "c", filter: null}` is not.
pub fn require_object_expected(doc: &Document, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Document(_)) => Ok(()),
        Some(_) => Err(type_mismatch(format!(
            "Expected field {field}to be of type object"
        ))),
    }
}

/// `Field '<name>' should be a boolean value, but found: <t>` — a strict bool
/// that rejects numbers and an explicit null.
pub fn require_bool_value(doc: &Document, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Boolean(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "Field '{field}' should be a boolean value, but found: {}",
            bson_type_name(v)
        ))),
    }
}

/// A strict-bool BSON-field slot (`update.updates.multi`). An explicit null is
/// accepted, unlike [`require_bool_value`].
pub fn require_bool_field(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) | Some(Bson::Boolean(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'bool'",
            bson_type_name(v)
        ))),
    }
}

/// `findAndModify.upsert`: a bool OR any number. `upsert: 1` and even
/// `upsert: 1.5` are valid, while the neighbouring `update.updates.multi`
/// rejects `multi: 1`. Two adjacent boolean-looking slots, two rules.
pub fn require_bool_or_number(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None
        | Some(Bson::Null)
        | Some(Bson::Boolean(_))
        | Some(Bson::Int32(_))
        | Some(Bson::Int64(_))
        | Some(Bson::Double(_))
        | Some(Bson::Decimal128(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected types {BOOL_OR_NUMBER_TYPES}",
            bson_type_name(v)
        ))),
    }
}

/// `maxTimeMS` — the only slot in the sweep that is not a `TypeMismatch`:
/// code 2, with three distinct messages. `Decimal128` is accepted; an explicit
/// null is rejected while absent is fine. No upper bound is enforced (unprobed).
pub fn require_max_time_ms(doc: &Document) -> Result<(), CommandError> {
    let value = match doc.get("maxTimeMS") {
        None => return Ok(()),
        Some(v) => v,
    };
    let number = match value {
        Bson::Int32(n) => f64::from(*n),
        Bson::Int64(n) => *n as f64,
        Bson::Double(d) => *d,
        // Decimal128 has no lossless f64 view here; mongod accepts it, and the
        // integral / range checks below are what a bad one would trip. Parsing
        // its string form keeps this faithful without a decimal dependency.
        Bson::Decimal128(d) => match d.to_string().parse::<f64>() {
            Ok(n) => n,
            Err(_) => return Ok(()),
        },
        _ => return Err(bad_value("maxTimeMS must be a number")),
    };
    if number.fract() != 0.0 {
        return Err(bad_value("maxTimeMS has non-integral value"));
    }
    if number < 0.0 {
        return Err(bad_value(format!(
            "{} value for maxTimeMS is out of range",
            number as i64
        )));
    }
    Ok(())
}

/// `aggregate`'s cursor slot: mongod's own wording, and it means "missing"
/// literally — an explicit `cursor: null` is rejected.
pub fn require_cursor_object(doc: &Document) -> Result<(), CommandError> {
    match doc.get("cursor") {
        None | Some(Bson::Document(_)) => Ok(()),
        Some(_) => Err(type_mismatch("cursor field must be missing or an object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn object_slot_accepts_absent_and_null_but_not_a_scalar() {
        assert!(require_object(&doc! {}, "let", "aggregate.let").is_ok());
        assert!(require_object(&doc! {"let": Bson::Null}, "let", "aggregate.let").is_ok());
        let err = require_object(&doc! {"let": 5}, "let", "aggregate.let").unwrap_err();
        assert_eq!(err.code, 14);
        assert_eq!(
            err.errmsg,
            "BSON field 'aggregate.let' is the wrong type 'int', expected type 'object'"
        );
    }

    #[test]
    fn expected_field_family_rejects_an_explicit_null() {
        // The difference from `require_object` that a shared helper would erase.
        assert!(require_object_expected(&doc! {}, "collation").is_ok());
        let err =
            require_object_expected(&doc! {"collation": Bson::Null}, "collation").unwrap_err();
        assert_eq!(err.errmsg, "Expected field collationto be of type object");
    }

    #[test]
    fn a_bool_is_not_a_number() {
        let err =
            require_number(&doc! {"limit": true}, "limit", "FindCommandRequest.limit").unwrap_err();
        assert!(err.errmsg.contains("wrong type 'bool'"), "{}", err.errmsg);
    }

    #[test]
    fn upsert_takes_numbers_but_multi_does_not() {
        // The asymmetry that makes this per-slot.
        assert!(
            require_bool_or_number(&doc! {"upsert": 1}, "upsert", "findAndModify.upsert").is_ok()
        );
        assert!(
            require_bool_or_number(&doc! {"upsert": 1.5}, "upsert", "findAndModify.upsert").is_ok()
        );
        let err =
            require_bool_field(&doc! {"multi": 1}, "multi", "update.updates.multi").unwrap_err();
        assert_eq!(
            err.errmsg,
            "BSON field 'update.updates.multi' is the wrong type 'int', expected type 'bool'"
        );
    }

    #[test]
    fn single_batch_rejects_numbers_and_null() {
        for (value, name) in [(Bson::Int32(1), "int"), (Bson::Null, "null")] {
            let err = require_bool_value(&doc! {"singleBatch": value}, "singleBatch").unwrap_err();
            assert_eq!(
                err.errmsg,
                format!("Field 'singleBatch' should be a boolean value, but found: {name}")
            );
        }
    }

    #[test]
    fn max_time_ms_has_three_messages_and_is_code_2() {
        let cases = [
            (doc! {"maxTimeMS": "x"}, "maxTimeMS must be a number"),
            (doc! {"maxTimeMS": true}, "maxTimeMS must be a number"),
            (doc! {"maxTimeMS": Bson::Null}, "maxTimeMS must be a number"),
            (doc! {"maxTimeMS": 1.5}, "maxTimeMS has non-integral value"),
            (
                doc! {"maxTimeMS": -1},
                "-1 value for maxTimeMS is out of range",
            ),
        ];
        for (d, expected) in cases {
            let err = require_max_time_ms(&d).unwrap_err();
            assert_eq!(err.code, 2, "{expected}");
            assert_eq!(err.errmsg, expected);
        }
        assert!(require_max_time_ms(&doc! {}).is_ok());
        assert!(require_max_time_ms(&doc! {"maxTimeMS": 0}).is_ok());
        assert!(require_max_time_ms(&doc! {"maxTimeMS": 5.0}).is_ok());
    }

    #[test]
    fn cursor_means_missing_not_null() {
        assert!(require_cursor_object(&doc! {}).is_ok());
        let err = require_cursor_object(&doc! {"cursor": Bson::Null}).unwrap_err();
        assert_eq!(err.errmsg, "cursor field must be missing or an object");
    }
}

/// mongod's rendering of a stage argument inside an error message: strings are
/// quoted, bools lowercase, arrays spaced (`[ 1 ]`), an empty document `{}`.
/// Probed on 6.0.16 via `$skip`'s message, which echoes the offending value.
fn render_stage_value(v: &Bson) -> String {
    match v {
        Bson::String(s) => format!("\"{s}\""),
        Bson::Boolean(b) => b.to_string(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(d) => {
            if d.fract() == 0.0 {
                format!("{d:.1}")
            } else {
                d.to_string()
            }
        }
        Bson::Null => "null".to_string(),
        // Without this a decimal rendered as its debug form,
        // `Decimal128(0f00...)`, instead of `1.5`.
        Bson::Decimal128(d) => d.to_string(),
        // Single quotes and a millisecond epoch -- mongod's shell forms, not
        // Rust's debug ones (`ObjectId("…")` / `DateTime(2026-01-02 …)`).
        Bson::ObjectId(oid) => format!("ObjectId('{oid}')"),
        Bson::DateTime(d) => format!("new Date({})", d.timestamp_millis()),
        Bson::Array(a) if a.is_empty() => "[]".to_string(),
        Bson::Array(a) => format!(
            "[ {} ]",
            a.iter()
                .map(render_stage_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Bson::Document(d) if d.is_empty() => "{}".to_string(),
        Bson::Document(d) => format!(
            "{{ {} }}",
            d.iter()
                .map(|(k, v)| format!("{k}: {}", render_stage_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}

/// A malformed aggregation STAGE spec, named with mongod's own code.
///
/// This is the `update::arith_type_error` pattern applied to the pipeline: a
/// standalone validator that names the errors we *can* name, leaving the engine's
/// generic `Fallback` for constructs it genuinely cannot do. On the Python server
/// `Fallback` means "the pure engine runs instead"; this server has no Python, so
/// an unnamed `Fallback` surfaces as a generic `BadValue` (2) — which is what all
/// 24 of these answered before.
///
/// Returns `(code, errmsg)` for the first offending stage, or `None` to leave the
/// pipeline alone.
pub fn stage_spec_error(pipeline: &[Bson]) -> Option<(i32, String)> {
    for stage in pipeline {
        let Bson::Document(stage) = stage else {
            continue; // a non-document ELEMENT is a different error, raised upstream
        };
        let Some((name, spec)) = stage.iter().next() else {
            continue;
        };
        let err = match name.as_str() {
            "$match" if !matches!(spec, Bson::Document(_)) => Some((
                15959,
                "the match filter must be an expression in an object".to_string(),
            )),
            "$sort" if !matches!(spec, Bson::Document(_)) => Some((
                15973,
                "the $sort key specification must be an object".to_string(),
            )),
            "$group" if !matches!(spec, Bson::Document(_)) => Some((
                15947,
                "a group's fields must be specified in an object".to_string(),
            )),
            "$lookup" if !matches!(spec, Bson::Document(_)) => Some((
                9,
                format!(
                    "the $lookup stage specification must be an object, but found {}",
                    bson_type_name(spec)
                ),
            )),
            "$count" if !matches!(spec, Bson::String(_)) => Some((
                40156,
                "the count field must be a non-empty string".to_string(),
            )),
            "$limit" | "$skip" => {
                // 5107201 / 5107200, and THREE cases, not one -- the first cut of
                // this shipped only the type check, so `$skip: 1.5` and
                // `$skip: -1` still fell through to the engine's Fallback and
                // answered a generic BadValue. The sweep did not catch it because
                // its corpus feeds only `{}` / `"x"` / `[1]`; the probe's reach is
                // exactly its case list. Decimal128 IS a number here (mongod
                // accepts `$skip: Decimal128("2")` -- probed).
                let code = if name == "$limit" { 5107201 } else { 5107200 };
                let n = match spec {
                    Bson::Int32(v) => f64::from(*v),
                    Bson::Int64(v) => *v as f64,
                    Bson::Double(v) => *v,
                    Bson::Decimal128(d) => d.to_string().parse::<f64>().unwrap_or(f64::NAN),
                    _ => {
                        return Some((
                            code,
                            format!(
                                "invalid argument to {name} stage: Expected a number in: \
                                 {name}: {}",
                                render_stage_value(spec)
                            ),
                        ));
                    }
                };
                if n.fract() != 0.0 {
                    // A decimal gets its own wording here (probed 6.0.16):
                    // 1.5 is "Expected an integer", Decimal128("1.5") is
                    // "Cannot represent as a 64-bit integer".
                    let reason = if matches!(spec, Bson::Decimal128(_)) {
                        "Cannot represent as a 64-bit integer"
                    } else {
                        "Expected an integer"
                    };
                    Some((
                        code,
                        format!(
                            "invalid argument to {name} stage: {reason}: {name}: {}",
                            render_stage_value(spec)
                        ),
                    ))
                } else if name == "$limit" && n == 0.0 {
                    // The engine defers this with a comment reading "Python
                    // raises 15958" -- true on that server, meaningless here,
                    // where the deferral became a generic BadValue. Name it.
                    Some((15958, "the limit must be positive".to_string()))
                } else if n < 0.0 {
                    Some((
                        code,
                        format!(
                            "invalid argument to {name} stage: Expected a non-negative number \
                             in: {name}: {}",
                            render_stage_value(spec)
                        ),
                    ))
                } else {
                    None
                }
            }
            "$unwind" => match spec {
                Bson::String(s) if s.is_empty() => {
                    Some((28812, "no path specified to $unwind stage".to_string()))
                }
                Bson::String(_) => None,
                Bson::Document(d) if !d.contains_key("path") => {
                    Some((28812, "no path specified to $unwind stage".to_string()))
                }
                Bson::Document(_) => None,
                other => Some((
                    15981,
                    format!(
                        "expected either a string or an object as specification for \
                         $unwind stage, got {}",
                        bson_type_name(other)
                    ),
                )),
            },
            _ => None,
        };
        if err.is_some() {
            return err;
        }
    }
    None
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    use bson::doc;

    fn err_for(stage: &str, spec: Bson) -> (i32, String) {
        let mut d = bson::Document::new();
        d.insert(stage, spec);
        stage_spec_error(&[Bson::Document(d)]).expect("expected an error")
    }

    #[test]
    fn each_stage_reports_its_own_code() {
        assert_eq!(err_for("$match", Bson::Int32(5)).0, 15959);
        assert_eq!(err_for("$sort", Bson::Int32(5)).0, 15973);
        assert_eq!(err_for("$group", Bson::Int32(5)).0, 15947);
        assert_eq!(err_for("$lookup", Bson::Int32(5)).0, 9);
        assert_eq!(err_for("$count", Bson::Int32(5)).0, 40156);
        assert_eq!(err_for("$limit", Bson::String("x".into())).0, 5107201);
        assert_eq!(err_for("$skip", Bson::String("x".into())).0, 5107200);
        assert_eq!(err_for("$unwind", Bson::Int32(5)).0, 15981);
    }

    #[test]
    fn lookup_and_unwind_name_the_offending_type() {
        assert_eq!(
            err_for("$lookup", Bson::String("x".into())).1,
            "the $lookup stage specification must be an object, but found string"
        );
        assert_eq!(
            err_for("$unwind", Bson::Array(vec![Bson::Int32(1)])).1,
            "expected either a string or an object as specification for $unwind stage, got array"
        );
    }

    #[test]
    fn skip_echoes_the_value_the_way_mongod_renders_it() {
        // Probed on 6.0.16: strings quoted, bools lowercase, arrays spaced.
        assert_eq!(
            err_for("$skip", Bson::String("x".into())).1,
            "invalid argument to $skip stage: Expected a number in: $skip: \"x\""
        );
        assert_eq!(
            err_for("$skip", Bson::Boolean(true)).1,
            "invalid argument to $skip stage: Expected a number in: $skip: true"
        );
        assert_eq!(
            err_for("$skip", Bson::Array(vec![Bson::Int32(1)])).1,
            "invalid argument to $skip stage: Expected a number in: $skip: [ 1 ]"
        );
        assert_eq!(
            err_for("$skip", Bson::Document(doc! {})).1,
            "invalid argument to $skip stage: Expected a number in: $skip: {}"
        );
        // Non-empty containers render recursively, shell-style.
        assert_eq!(
            err_for("$skip", Bson::Document(doc! {"a": 1})).1,
            "invalid argument to $skip stage: Expected a number in: $skip: { a: 1 }"
        );
        assert_eq!(
            err_for(
                "$skip",
                Bson::Array(vec![Bson::Int32(1), Bson::String("a".into())])
            )
            .1,
            "invalid argument to $skip stage: Expected a number in: $skip: [ 1, \"a\" ]"
        );
    }

    #[test]
    fn unwind_distinguishes_no_path_from_a_wrong_type() {
        assert_eq!(err_for("$unwind", Bson::Document(doc! {})).0, 28812);
        assert_eq!(err_for("$unwind", Bson::String("".into())).0, 28812);
        // A well-formed spec is left alone.
        assert!(stage_spec_error(&[Bson::Document(doc! {"$unwind": "$a"})]).is_none());
        assert!(stage_spec_error(&[Bson::Document(doc! {"$unwind": {"path": "$a"}})]).is_none());
    }

    #[test]
    fn limit_and_skip_have_three_cases_not_one() {
        // The first cut shipped only the type check, so these two fell through
        // to the engine's Fallback and answered a generic BadValue. The sweep
        // corpus feeds only {} / "x" / [1], so it passed anyway.
        assert_eq!(
            err_for("$skip", Bson::Double(1.5)),
            (
                5107200,
                "invalid argument to $skip stage: Expected an integer: $skip: 1.5".to_string()
            )
        );
        assert_eq!(
            err_for("$skip", Bson::Int32(-1)),
            (
                5107200,
                "invalid argument to $skip stage: Expected a non-negative number in: $skip: -1"
                    .to_string()
            )
        );
        assert_eq!(
            err_for("$limit", Bson::Double(-2.0)).1,
            "invalid argument to $limit stage: Expected a non-negative number in: $limit: -2.0"
        );
    }

    #[test]
    fn a_zero_limit_is_named_not_deferred() {
        assert_eq!(
            err_for("$limit", Bson::Int32(0)),
            (15958, "the limit must be positive".to_string())
        );
        // $skip: 0 is perfectly legal, and must stay so.
        assert!(stage_spec_error(&[Bson::Document(doc! {"$skip": 0})]).is_none());
    }

    #[test]
    fn a_decimal_is_a_number_here() {
        // mongod ACCEPTS `$skip: Decimal128("2")` -- probed. Both servers used
        // to reject it.
        let two: bson::Decimal128 = "2".parse().unwrap();
        assert!(stage_spec_error(&[Bson::Document(doc! {"$skip": two})]).is_none());
        let frac: bson::Decimal128 = "1.5".parse().unwrap();
        assert_eq!(err_for("$skip", Bson::Decimal128(frac)).0, 5107200);
    }

    #[test]
    fn well_formed_stages_are_untouched() {
        let pipeline = vec![
            Bson::Document(doc! {"$match": {"a": 1}}),
            Bson::Document(doc! {"$sort": {"a": 1}}),
            Bson::Document(doc! {"$limit": 5}),
            Bson::Document(doc! {"$skip": 1.0}),
            Bson::Document(doc! {"$count": "n"}),
        ];
        assert!(stage_spec_error(&pipeline).is_none());
    }
}

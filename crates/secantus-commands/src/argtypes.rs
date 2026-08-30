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

/// mongod's numeric-slot type list, probed on 8.2.1. The ORDER is mongod's own
/// and differs per field, so this is copied rather than derived. (6.0.16 also
/// put the closing quote inside the bracket here; 8.x quotes it properly.)
/// Mirrors `commands._NUMERIC_TYPES_MSG`.
const NUMERIC_TYPES: &str = "'[decimal, int, double, long]'";

/// `findAndModify.upsert` accepts a bool OR any number, unlike the strict-bool
/// `update.updates.multi` beside it. Probed 8.2.1; note the order differs from
/// `NUMERIC_TYPES` above. Mirrors `commands._BOOL_OR_NUMBER_TYPES_MSG`.
const BOOL_OR_NUMBER_TYPES: &str = "'[int, decimal, long, bool, double]'";

fn type_mismatch(errmsg: impl Into<String>) -> CommandError {
    CommandError::new(14, "TypeMismatch", errmsg.into())
}

fn bad_value(errmsg: impl Into<String>) -> CommandError {
    CommandError::new(2, "BadValue", errmsg.into())
}

fn failed_to_parse(errmsg: impl Into<String>) -> CommandError {
    CommandError::new(9, "FailedToParse", errmsg.into())
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

/// An index-spec BOOLEAN option (`unique` / `sparse`), whose message is not the
/// one [`require_index_spec_field`] produces beside it.
///
/// mongod's own quoting is broken here and is reproduced verbatim: the opening
/// quote before the field name is never closed — `The field 'unique has value
/// unique: {}, which is not convertible to bool`. A number IS convertible, so
/// `unique: 1.5` is accepted where `unique: "x"` is not.
pub fn require_index_spec_bool(spec: &Document, field: &str) -> Result<(), CommandError> {
    match spec.get(field) {
        None
        | Some(Bson::Boolean(_))
        | Some(Bson::Int32(_))
        | Some(Bson::Int64(_))
        | Some(Bson::Double(_))
        | Some(Bson::Decimal128(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "Error in specification {} :: caused by :: The field '{field} has value {field}: {}, \
             which is not convertible to bool",
            render_stage_value(&Bson::Document(spec.clone())),
            render_stage_value(v)
        ))),
    }
}

/// `createIndexes`'s `expireAfterSeconds`: code 67 (CannotCreateIndex), a
/// leading `". Index spec: "` (mongod's own dangling full stop, from an empty
/// namespace prefix) and, again, an unclosed quote around the type name.
pub fn require_index_spec_ttl(spec: &Document, field: &str) -> Result<(), CommandError> {
    match spec.get(field) {
        None
        | Some(Bson::Int32(_))
        | Some(Bson::Int64(_))
        | Some(Bson::Double(_))
        | Some(Bson::Decimal128(_)) => Ok(()),
        Some(v) => Err(CommandError::new(
            67,
            "CannotCreateIndex",
            format!(
                ". Index spec: {} :: caused by :: TTL index '{field}' option must be numeric, \
                 but received a type of '{}",
                render_stage_value(&Bson::Document(spec.clone())),
                bson_type_name(v)
            ),
        )),
    }
}

/// A `$[<identifier>]` in an update path with no matching `arrayFilters` entry:
/// `No array filter found for identifier 'e' in path 'a.$[e]'` (BadValue, 2).
///
/// mongod decides this from the update document ALONE, before touching a
/// document — so it fires even when the field is not an array. That is exactly
/// the case this server got wrong: the engine's walk returns early for a
/// non-array value, so `{$set: {"a.$[e]": 1}}` with no filters reported ok:1
/// against `{a: 1}` and wrote nothing. Checked here, at the command layer,
/// rather than in the engine, because the engine's only failure signal is
/// `Fallback` — which on a server with no Python becomes a generic BadValue
/// with the wrong text. Same template as [`stage_spec_error`].
pub fn array_filter_identifier_error(
    update: &Document,
    filters: &[Document],
) -> Option<CommandError> {
    let named: Vec<String> = filters
        .iter()
        .flat_map(|f| f.keys())
        .map(|k| k.split('.').next().unwrap_or(k).to_string())
        .collect();
    for value in update.values() {
        let Bson::Document(payload) = value else {
            continue;
        };
        for path in payload.keys() {
            for part in path.split('.') {
                if let Some(name) = part.strip_prefix("$[").and_then(|p| p.strip_suffix(']')) {
                    if !name.is_empty() && !named.iter().any(|n| n == name) {
                        return Some(bad_value(format!(
                            "No array filter found for identifier '{name}' in path '{path}'"
                        )));
                    }
                }
            }
        }
    }
    None
}

/// `Hint must be a string or an object` (FailedToParse, 9).
///
/// Not the BSON-field family: no field name, no type name, and an explicit
/// `null` is rejected like any other wrong type. The same message serves
/// `find` / `count` / `aggregate` / `update` / `delete` / `findAndModify`, so
/// one validator covers six commands. A *string* hint naming no index is a
/// different, later error (the planner's), not this one.
pub fn require_hint(doc: &Document, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::String(_)) | Some(Bson::Document(_)) => Ok(()),
        Some(_) => Err(failed_to_parse("Hint must be a string or an object")),
    }
}

/// `renameCollection.dropTarget`: a bool OR binData. The binData half looks
/// like a mistake and is mongod's, measured on 8.2.11 — it is the type list
/// the IDL emits, so fidelity means carrying it.
pub fn require_bool_or_bindata(
    doc: &Document,
    field: &str,
    path: &str,
) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::Boolean(_)) | Some(Bson::Binary(_)) => Ok(()),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected types '[bool, binData]'",
            bson_type_name(v)
        ))),
    }
}

/// A required string slot, where an explicit `null` means ABSENT rather than
/// wrong-typed and answers 40414 instead of 14 — the same null-means-absent
/// rule `createIndexes.indexes` follows. `getMore.collection` and
/// `renameCollection.to` share it.
pub fn require_required_string(
    doc: &Document,
    field: &str,
    path: &str,
) -> Result<(), CommandError> {
    match doc.get(field) {
        Some(Bson::String(_)) => Ok(()),
        None | Some(Bson::Null) => Err(CommandError::new(
            40414,
            "Location40414",
            format!("BSON field '{path}' is missing but a required field"),
        )),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'string'",
            bson_type_name(v)
        ))),
    }
}

/// The array counterpart of [`require_required_string`] (`killCursors.cursors`).
pub fn require_required_array(doc: &Document, field: &str, path: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        Some(Bson::Array(_)) => Ok(()),
        None | Some(Bson::Null) => Err(CommandError::new(
            40414,
            "Location40414",
            format!("BSON field '{path}' is missing but a required field"),
        )),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected type 'array'",
            bson_type_name(v)
        ))),
    }
}

/// `dropIndexes.index`: a string (a name) or an object (a key pattern).
///
/// The type list in the message is not constant — an ARRAY is told the expected
/// type is `'[string]'` while every other wrong type is told `'[string, object]'`.
/// Measured on 8.2.11, not derived; an array is presumably reaching a different
/// IDL overload. A document falls through to the index lookup, which answers 27.
pub fn require_index_name_or_key(
    doc: &Document,
    field: &str,
    path: &str,
) -> Result<(), CommandError> {
    match doc.get(field) {
        None | Some(Bson::String(_)) | Some(Bson::Document(_)) => Ok(()),
        Some(v @ Bson::Array(_)) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected types '[string]'",
            bson_type_name(v)
        ))),
        Some(v) => Err(type_mismatch(format!(
            "BSON field '{path}' is the wrong type '{}', expected types '[string, object]'",
            bson_type_name(v)
        ))),
    }
}

/// `count.limit`'s value check: `limit value is not a valid number`
/// (BadValue, 2).
///
/// `count.limit` does NOT use the BSON-field family its neighbours use, and it
/// rejects an explicit `null` where that family accepts one. Probed on 8.2.11 —
/// `count.skip` right beside it answers the ordinary numeric type error, so two
/// adjacent slots on one command take two different families.
pub fn require_count_limit(doc: &Document, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        None
        | Some(Bson::Int32(_))
        | Some(Bson::Int64(_))
        | Some(Bson::Double(_))
        | Some(Bson::Decimal128(_)) => Ok(()),
        Some(_) => Err(bad_value(format!("{field} value is not a valid number"))),
    }
}

/// `createIndexes.indexes`: on 8.x an explicit `null` means the required field
/// is ABSENT, and answers exactly what omitting it answers — the same
/// null-means-absent rule as `findAndModify.arrayFilters` and
/// `killCursors.cursors`. Both servers used to answer the 6.0 form
/// (`10065 invalid parameter: expected an object (indexes)`); re-probed 8.2.1.
/// A wrong-TYPED non-null is still the ordinary array type error (14), so this
/// slot has two codes split by null-ness — just not the two it used to have.
pub fn require_index_specs(doc: &Document) -> Result<(), CommandError> {
    match doc.get("indexes") {
        Some(Bson::Array(_)) => Ok(()),
        None | Some(Bson::Null) => Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field 'createIndexes.indexes' is missing but a required field",
        )),
        Some(v) => Err(type_mismatch(format!(
            "BSON field 'createIndexes.indexes' is the wrong type '{}', expected type 'array'",
            bson_type_name(v)
        ))),
    }
}

/// A field inside one `createIndexes` spec. mongod quotes the WHOLE spec back
/// and chains the reason, a message family of its own:
///
/// ```text
/// Error in specification { key: null, name: "i" } :: caused by ::
///   The field 'key' must be an object, but got null
/// ```
pub fn require_index_spec_field(
    spec: &Document,
    field: &str,
    expected: &str,
    ok: impl Fn(&Bson) -> bool,
) -> Result<(), CommandError> {
    let value = spec.get(field).unwrap_or(&Bson::Null);
    if !matches!(spec.get(field), Some(Bson::Null) | None) && ok(value) {
        return Ok(());
    }
    if spec.get(field).is_none() {
        return Ok(()); // absent is a different error, raised downstream
    }
    Err(type_mismatch(format!(
        "Error in specification {} :: caused by :: The field '{field}' must be {expected}, \
         but got {}",
        render_stage_value(&Bson::Document(spec.clone())),
        bson_type_name(value)
    )))
}

/// mongod names the IDL *struct* in a `maxTimeMS` type error. Probed across 24
/// commands on 8.2.1: for every one of them that struct is simply the command
/// name — with exactly one exception, which is why this is a lookup and not a
/// format string. Mirrors `commands._MAX_TIME_MS_STRUCTS`.
fn max_time_ms_struct(command: &str) -> &str {
    if command == "find" {
        "FindCommandRequest"
    } else {
        command
    }
}

/// mongod's own ceiling for the slot, reported verbatim in the range message.
const MAX_TIME_MS_LIMIT: i64 = i32::MAX as i64;

/// `maxTimeMS` on any command, as mongod 8.2.1 validates it.
///
/// Three checks in a fixed order, each with its own code. **The order is
/// load-bearing**: `-1.5` is both non-integral and negative, and mongod answers
/// the integral error (9), not the range one (2).
///
/// ```text
/// "x" / {} / [1] / true   14  BSON field '<struct>.maxTimeMS' is the wrong type
///                             '<t>', expected types '[decimal, int, double, long]'
/// 1.5 / -1.5 / -0.5       9   Expected an integer: maxTimeMS: 1.5
/// double NaN              9   Expected an integer, but found NaN in: maxTimeMS: nan
/// double 1e100 / inf      9   Cannot represent as a 64-bit integer: maxTimeMS: 1e+100
/// Decimal128 non-integral 9   Cannot represent as a 64-bit integer: maxTimeMS: 1.5
/// -1                      2   BSON field 'maxTimeMS' value must be >= 0, actual value '-1'
/// 2**31                   2   BSON field 'maxTimeMS' value must be <= 2147483647, ...
/// null / 0 / 2147483647   accepted
/// ```
///
/// The range message carries NO struct prefix where the type message does; that
/// asymmetry is mongod's. A fractional `double` and a fractional `Decimal128`
/// also get different wording for the same numeric value.
///
/// This replaces a port of the 6.0 contract whose doc comment called the slot
/// "the only one in the sweep that is not a TypeMismatch" — 8.x honours none of
/// its four behaviours, and an explicit null is now ACCEPTED (it means absent).
/// It was also called from `find` / `aggregate` / `findAndModify` only, so the
/// other 21 commands took a wrong-typed value silently; it now runs in
/// `dispatch`. Mirrors `commands._require_max_time_ms`.
pub fn require_max_time_ms(doc: &Document, command: &str) -> Result<(), CommandError> {
    let value = match doc.get("maxTimeMS") {
        // 8.x treats an explicit null as the field being absent.
        None | Some(Bson::Null) => return Ok(()),
        Some(v) => v,
    };
    let number: i64 = match value {
        Bson::Int32(n) => i64::from(*n),
        Bson::Int64(n) => *n,
        Bson::Double(d) => {
            if d.is_nan() {
                return Err(failed_to_parse(format!(
                    "Expected an integer, but found NaN in: maxTimeMS: {}",
                    render_double(*d)
                )));
            }
            if d.is_infinite() || *d < i64::MIN as f64 || *d > i64::MAX as f64 {
                return Err(failed_to_parse(format!(
                    "Cannot represent as a 64-bit integer: maxTimeMS: {}",
                    render_double(*d)
                )));
            }
            if d.fract() != 0.0 {
                return Err(failed_to_parse(format!(
                    "Expected an integer: maxTimeMS: {}",
                    render_double(*d)
                )));
            }
            *d as i64
        }
        // The literal the client sent is echoed back verbatim (`1E+40`, not
        // `1e+40`), so the string form is what goes in the message.
        Bson::Decimal128(dec) => {
            let text = dec.to_string();
            match text.parse::<f64>() {
                Ok(n)
                    if n.is_finite()
                        && n.fract() == 0.0
                        && (i64::MIN as f64..=i64::MAX as f64).contains(&n) =>
                {
                    n as i64
                }
                _ => {
                    return Err(failed_to_parse(format!(
                        "Cannot represent as a 64-bit integer: maxTimeMS: {text}"
                    )))
                }
            }
        }
        _ => {
            return Err(type_mismatch(format!(
                "BSON field '{}.maxTimeMS' is the wrong type '{}', expected types {NUMERIC_TYPES}",
                max_time_ms_struct(command),
                bson_type_name(value)
            )))
        }
    };
    if number < 0 {
        return Err(bad_value(format!(
            "BSON field 'maxTimeMS' value must be >= 0, actual value '{number}'"
        )));
    }
    if number > MAX_TIME_MS_LIMIT {
        return Err(bad_value(format!(
            "BSON field 'maxTimeMS' value must be <= {MAX_TIME_MS_LIMIT}, actual value '{number}'"
        )));
    }
    Ok(())
}

/// mongod renders these the way C++ does, which for every case probed matches
/// Rust's own `{}` for a float except that Rust prints `inf` / `NaN` and an
/// integral-valued double without an exponent. Only the non-integral and
/// non-finite paths reach this, so `1.5`, `-0.5`, `1e100`, `inf` and `nan` are
/// what matter.
fn render_double(d: f64) -> String {
    if d.is_nan() {
        return "nan".to_string();
    }
    if d.is_infinite() {
        return if d > 0.0 { "inf".into() } else { "-inf".into() };
    }
    // C++ iostreams switch to scientific past 6 significant digits; the values
    // that get here are either small fractions or astronomically large.
    if d != 0.0 && (d.abs() >= 1e16 || d.abs() < 1e-4) {
        let s = format!("{d:e}");
        // Rust writes `1e100`; C++ writes `1e+100`.
        return match s.split_once('e') {
            Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
            _ => s,
        };
    }
    format!("{d}")
}

/// `aggregate`'s cursor slot: mongod's own wording, and it means "missing"
/// literally — an explicit `cursor: null` is rejected.
pub fn require_cursor_object(doc: &Document) -> Result<(), CommandError> {
    match doc.get("cursor") {
        None | Some(Bson::Document(_)) => Ok(()),
        Some(_) => Err(type_mismatch("cursor field must be missing or an object")),
    }
}

/// `listIndexes.cursor`, which — unlike `aggregate.cursor` two functions up —
/// ACCEPTS an explicit `null` (probed 6.0.16). The same option name on two
/// commands with two rules; sharing one validator was wrong.
pub fn require_cursor_object_nullable(doc: &Document) -> Result<(), CommandError> {
    match doc.get("cursor") {
        None | Some(Bson::Null) | Some(Bson::Document(_)) => Ok(()),
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
    fn max_time_ms_wrong_type_is_14_and_names_the_idl_struct() {
        // The struct is the command name for every command but `find`.
        for (command, struct_name) in [
            ("find", "FindCommandRequest"),
            ("aggregate", "aggregate"),
            ("count", "count"),
            ("ping", "ping"),
        ] {
            for bad in [
                Bson::String("x".into()),
                Bson::Boolean(true),
                Bson::Document(doc! {}),
                Bson::Array(vec![Bson::Int32(1)]),
            ] {
                let err =
                    require_max_time_ms(&doc! {"maxTimeMS": bad.clone()}, command).unwrap_err();
                assert_eq!(err.code, 14, "{command} {bad:?}");
                assert!(
                    err.errmsg
                        .starts_with(&format!("BSON field '{struct_name}.maxTimeMS'")),
                    "{}",
                    err.errmsg
                );
                assert!(err
                    .errmsg
                    .ends_with(&format!("expected types {NUMERIC_TYPES}")));
            }
        }
    }

    #[test]
    fn max_time_ms_non_integral_is_9_and_splits_by_bson_type() {
        // A fractional double and a fractional Decimal128 do NOT share wording.
        let cases: [(Bson, &str); 7] = [
            (Bson::Double(1.5), "Expected an integer: maxTimeMS: 1.5"),
            (Bson::Double(-1.5), "Expected an integer: maxTimeMS: -1.5"),
            (Bson::Double(-0.5), "Expected an integer: maxTimeMS: -0.5"),
            (
                Bson::Double(f64::NAN),
                "Expected an integer, but found NaN in: maxTimeMS: nan",
            ),
            (
                Bson::Double(f64::INFINITY),
                "Cannot represent as a 64-bit integer: maxTimeMS: inf",
            ),
            (
                Bson::Double(1e100),
                "Cannot represent as a 64-bit integer: maxTimeMS: 1e+100",
            ),
            (
                Bson::Decimal128("1.5".parse().unwrap()),
                "Cannot represent as a 64-bit integer: maxTimeMS: 1.5",
            ),
        ];
        for (value, expected) in cases {
            let err = require_max_time_ms(&doc! {"maxTimeMS": value}, "find").unwrap_err();
            assert_eq!(err.code, 9, "{expected}");
            assert_eq!(err.errmsg, expected);
        }
    }

    #[test]
    fn max_time_ms_out_of_range_is_2_without_the_struct_prefix() {
        let err = require_max_time_ms(&doc! {"maxTimeMS": -1}, "find").unwrap_err();
        assert_eq!(err.code, 2);
        assert_eq!(
            err.errmsg,
            "BSON field 'maxTimeMS' value must be >= 0, actual value '-1'"
        );
        let err = require_max_time_ms(&doc! {"maxTimeMS": 2147483648i64}, "find").unwrap_err();
        assert_eq!(err.code, 2);
        assert_eq!(
            err.errmsg,
            "BSON field 'maxTimeMS' value must be <= 2147483647, actual value '2147483648'"
        );
    }

    #[test]
    fn max_time_ms_negative_and_non_integral_answers_the_integral_error() {
        // The check ORDER: -1.5 is both, and mongod answers 9, not 2.
        let err = require_max_time_ms(&doc! {"maxTimeMS": -1.5}, "find").unwrap_err();
        assert_eq!(err.code, 9);
    }

    #[test]
    fn max_time_ms_accepts_absent_null_and_valid_numbers() {
        for d in [
            doc! {},
            // 8.x: an explicit null means the field was not sent. 6.0 rejected it.
            doc! {"maxTimeMS": Bson::Null},
            doc! {"maxTimeMS": 0},
            doc! {"maxTimeMS": 5000},
            doc! {"maxTimeMS": 5000.0},
            doc! {"maxTimeMS": 2147483647i64},
            doc! {"maxTimeMS": Bson::Decimal128("5000".parse().unwrap())},
        ] {
            assert!(require_max_time_ms(&d, "find").is_ok(), "{d:?}");
        }
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
pub(crate) fn render_stage_value(v: &Bson) -> String {
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
            // --- stages added by the 2026-08-31 wide sweep --------------------
            // Each carries mongod's own code AND its own wording; the family
            // looks uniform and is not. Four different verbs ("must be an
            // object" / "expected an object as" / "Argument to … must be"),
            // two different capitalisations of "The", and `$project` names no
            // type at all while its neighbours all do. Probed on 8.2.11.
            "$addFields" | "$set" if !matches!(spec, Bson::Document(_)) => Some((
                40272,
                format!(
                    "$addFields specification stage must be an object, got {}",
                    bson_type_name(spec)
                ),
            )),
            "$project" if !matches!(spec, Bson::Document(_)) => Some((
                15969,
                "$project specification must be an object".to_string(),
            )),
            "$replaceRoot" | "$replaceWith" if !matches!(spec, Bson::Document(_)) => Some((
                40229,
                format!(
                    "expected an object as specification for $replaceRoot stage, got {}",
                    bson_type_name(spec)
                ),
            )),
            // The only stage that echoes the offending VALUE rather than its
            // type, and it prefixes it with the stage name again.
            "$facet" if !matches!(spec, Bson::Document(_)) => Some((
                40169,
                format!(
                    "the $facet specification must be a non-empty object, but found: $facet: {}",
                    render_stage_value(spec)
                ),
            )),
            "$bucket" if !matches!(spec, Bson::Document(_)) => Some((
                40201,
                format!(
                    "Argument to $bucket stage must be an object, but found type: {}.",
                    bson_type_name(spec)
                ),
            )),
            // Three codes for one stage, split by what the spec IS rather than
            // by whether it is valid: a non-string non-object is 40149, a
            // string that is not $-prefixed is 40148, and an empty object is
            // 40147 — the same message text under two different codes.
            "$sortByCount" => match spec {
                Bson::String(s) if s.starts_with('$') => None,
                Bson::String(_) => Some((
                    40148,
                    "the sortByCount field must be defined as a $-prefixed path or an \
                     expression inside an object"
                        .to_string(),
                )),
                Bson::Document(d) if d.is_empty() => Some((
                    40147,
                    "the sortByCount field must be defined as a $-prefixed path or an \
                     expression inside an object"
                        .to_string(),
                )),
                Bson::Document(_) => None,
                _ => Some((
                    40149,
                    "the sortByCount field must be specified as a string or as an object"
                        .to_string(),
                )),
            },
            "$geoNear" if !matches!(spec, Bson::Document(_)) => Some((
                10065,
                "invalid parameter: expected an object ($geoNear)".to_string(),
            )),
            "$graphLookup" if !matches!(spec, Bson::Document(_)) => Some((
                9,
                format!(
                    "the $graphLookup stage specification must be an object, but found {}",
                    bson_type_name(spec)
                ),
            )),
            // Takes a string as well — the collection to union with.
            "$unionWith" if !matches!(spec, Bson::Document(_) | Bson::String(_)) => Some((
                9,
                format!(
                    "the $unionWith stage specification must be an object or string, \
                         but found {}",
                    bson_type_name(spec)
                ),
            )),
            "$setWindowFields" if !matches!(spec, Bson::Document(_)) => Some((
                9,
                format!(
                    "the $setWindowFields stage specification must be an object, found {}",
                    bson_type_name(spec)
                ),
            )),
            // Capital "The" on these two, lowercase "the" on $setWindowFields
            // directly above. mongod's own inconsistency; fidelity means keeping it.
            "$densify" if !matches!(spec, Bson::Document(_)) => Some((
                9,
                format!(
                    "The $densify stage specification must be an object, found {}",
                    bson_type_name(spec)
                ),
            )),
            "$fill" if !matches!(spec, Bson::Document(_)) => Some((
                9,
                format!(
                    "The $fill stage specification must be an object, found {}",
                    bson_type_name(spec)
                ),
            )),
            "$sample" if !matches!(spec, Bson::Document(_)) => Some((
                28745,
                "the $sample stage specification must be an object".to_string(),
            )),
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

    /// The 2026-08-31 sweep's stages. Each code is mongod 8.2.11's own; a
    /// blanket "spec must be an object" rule would give one code to all of them
    /// and match none.
    #[test]
    fn swept_stages_each_carry_their_own_code() {
        for (stage, code) in [
            ("$addFields", 40272),
            ("$set", 40272),
            ("$project", 15969),
            ("$replaceRoot", 40229),
            ("$replaceWith", 40229),
            ("$facet", 40169),
            ("$bucket", 40201),
            ("$geoNear", 10065),
            ("$graphLookup", 9),
            ("$unionWith", 9),
            ("$setWindowFields", 9),
            ("$densify", 9),
            ("$fill", 9),
            ("$sample", 28745),
        ] {
            assert_eq!(err_for(stage, Bson::Int32(5)).0, code, "{stage}");
        }
    }

    #[test]
    fn swept_stages_use_mongods_own_wording() {
        assert_eq!(
            err_for("$addFields", Bson::String("x".into())).1,
            "$addFields specification stage must be an object, got string"
        );
        assert_eq!(
            err_for("$bucket", Bson::Boolean(true)).1,
            "Argument to $bucket stage must be an object, but found type: bool."
        );
        // The only one that echoes the VALUE, and it repeats the stage name.
        assert_eq!(
            err_for("$facet", Bson::String("x".into())).1,
            "the $facet specification must be a non-empty object, but found: $facet: \"x\""
        );
        // Capital "The" here, lowercase on $setWindowFields — mongod's own
        // inconsistency, not a typo in the port.
        assert!(err_for("$densify", Bson::Int32(5)).1.starts_with("The $densify"));
        assert!(err_for("$setWindowFields", Bson::Int32(5))
            .1
            .starts_with("the $setWindowFields"));
    }

    #[test]
    fn union_with_takes_a_string_and_sort_by_count_splits_three_ways() {
        // $unionWith's string form names a collection, so it is valid.
        assert!(stage_spec_error(&[Bson::Document(doc! {"$unionWith": "other"})]).is_none());
        // $sortByCount: three codes, split by what the spec IS.
        assert_eq!(err_for("$sortByCount", Bson::Int32(5)).0, 40149);
        assert_eq!(err_for("$sortByCount", Bson::String("nodollar".into())).0, 40148);
        assert_eq!(err_for("$sortByCount", Bson::Document(Document::new())).0, 40147);
        assert!(stage_spec_error(&[Bson::Document(doc! {"$sortByCount": "$a"})]).is_none());
    }
}

#[cfg(test)]
mod swept_slot_tests {
    use super::*;
    use bson::doc;

    #[test]
    fn hint_rejects_everything_that_is_not_a_string_or_object() {
        assert!(require_hint(&doc! {"hint": "i_1"}, "hint").is_ok());
        assert!(require_hint(&doc! {"hint": {"a": 1}}, "hint").is_ok());
        assert!(require_hint(&doc! {}, "hint").is_ok());
        for bad in [Bson::Int32(5), Bson::Boolean(true), Bson::Null] {
            let err = require_hint(&doc! {"hint": bad}, "hint").unwrap_err();
            assert_eq!(err.code, 9);
            assert_eq!(err.errmsg, "Hint must be a string or an object");
        }
    }

    #[test]
    fn a_required_slot_reads_null_as_absent() {
        // 40414 (missing), not 14 (wrong type) -- the null-means-absent rule.
        for d in [doc! {"collection": Bson::Null}, doc! {}] {
            let err = require_required_string(&d, "collection", "getMore.collection").unwrap_err();
            assert_eq!(err.code, 40414);
            assert_eq!(
                err.errmsg,
                "BSON field 'getMore.collection' is missing but a required field"
            );
        }
        let err = require_required_string(&doc! {"collection": 5}, "collection", "getMore.collection")
            .unwrap_err();
        assert_eq!(err.code, 14);
    }

    #[test]
    fn count_limit_and_skip_take_different_families() {
        // Two adjacent numeric slots on one command, two rules: `limit`
        // rejects null with its own BadValue wording, `skip` accepts it.
        let err = require_count_limit(&doc! {"limit": Bson::Null}, "limit").unwrap_err();
        assert_eq!(err.code, 2);
        assert_eq!(err.errmsg, "limit value is not a valid number");
        assert!(require_number(&doc! {"skip": Bson::Null}, "skip", "count.skip").is_ok());
    }

    #[test]
    fn drop_indexes_names_a_different_type_list_for_an_array() {
        let err = require_index_name_or_key(&doc! {"index": [1]}, "index", "dropIndexes.index")
            .unwrap_err();
        assert!(err.errmsg.ends_with("expected types '[string]'"), "{}", err.errmsg);
        let err = require_index_name_or_key(&doc! {"index": 5}, "index", "dropIndexes.index")
            .unwrap_err();
        assert!(err.errmsg.ends_with("expected types '[string, object]'"), "{}", err.errmsg);
    }

    #[test]
    fn index_spec_bool_and_ttl_carry_mongods_broken_quoting() {
        // mongod never closes the quote it opens before the field name. This
        // asserts the defect on purpose: fidelity is the point.
        let spec = doc! {"key": {"a": 1}, "name": "i", "unique": "x"};
        let err = require_index_spec_bool(&spec, "unique").unwrap_err();
        assert!(
            err.errmsg.contains("The field 'unique has value unique: \"x\", which is not convertible to bool"),
            "{}",
            err.errmsg
        );
        // A number IS convertible, so this one is accepted.
        let ok = doc! {"key": {"a": 1}, "name": "i", "unique": 1.5};
        assert!(require_index_spec_bool(&ok, "unique").is_ok());

        let ttl = doc! {"key": {"a": 1}, "name": "i", "expireAfterSeconds": "x"};
        let err = require_index_spec_ttl(&ttl, "expireAfterSeconds").unwrap_err();
        assert_eq!(err.code, 67);
        assert!(err.errmsg.starts_with(". Index spec: "), "{}", err.errmsg);
    }

    #[test]
    fn an_unmatched_array_filter_identifier_is_named() {
        let update = doc! {"$set": {"a.$[e]": 1}};
        let err = array_filter_identifier_error(&update, &[]).unwrap();
        assert_eq!(err.code, 2);
        assert_eq!(
            err.errmsg,
            "No array filter found for identifier 'e' in path 'a.$[e]'"
        );
        // Present -> fine. `$[]` (no identifier) never needs a filter.
        let f = vec![doc! {"e.x": 1}];
        assert!(array_filter_identifier_error(&update, &f).is_none());
        assert!(array_filter_identifier_error(&doc! {"$set": {"a.$[]": 1}}, &[]).is_none());
    }
}

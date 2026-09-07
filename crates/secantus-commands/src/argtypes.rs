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

/// A write statement's `q` (and `u`) -- validated per STATEMENT, because a
/// malformed one used to fall through every match arm and the statement
/// reported success. mongod rejects the whole command (probed 8.2.11):
/// a non-document `q` is 14, a non-document/array `u` is 9, and an absent
/// one of either is 40414.
pub fn require_write_statement(spec: &Document, command: &str) -> Result<(), CommandError> {
    let container = if command == "delete" {
        "deletes"
    } else {
        "updates"
    };
    let path = |field: &str| format!("{command}.{container}.{field}");
    match spec.get("q") {
        None => {
            return Err(CommandError::new(
                40414,
                "IDLFailedToParse",
                format!("BSON field '{}' is missing but a required field", path("q")),
            ))
        }
        Some(Bson::Document(_)) => {}
        Some(v) => {
            return Err(type_mismatch(format!(
                "BSON field '{}' is the wrong type '{}', expected type 'object'",
                path("q"),
                bson_type_name(v)
            )))
        }
    }
    if command == "delete" {
        return Ok(());
    }
    match spec.get("u") {
        None => Err(CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{}' is missing but a required field", path("u")),
        )),
        Some(Bson::Document(_)) => Ok(()),
        // An array is a pipeline-form update, and its ELEMENTS are stages --
        // a non-document among them is 14, not the 9 that a non-array `u` gets.
        Some(Bson::Array(stages)) => {
            if stages.iter().all(|st| matches!(st, Bson::Document(_))) {
                Ok(())
            } else {
                Err(type_mismatch(
                    "Each element of the 'pipeline' array must be an object".to_string(),
                ))
            }
        }
        Some(_) => Err(CommandError::new(
            9,
            "FailedToParse",
            "Update argument must be either an object or an array",
        )),
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
    // Reuse the engine's extractor rather than reading the filter's top-level
    // keys: an identifier may be nested inside `$and` / `$or` / `$nor`
    // (`[{$and: [{"x.g": {$gt: 8}}]}]` names `x`), and a naive read reported
    // that valid filter as missing.
    let named: Vec<String> = filters
        .iter()
        .flat_map(secantus_core::update::extract_af_identifiers)
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
        // The SPEC rendering: shortest round-trip, a whole double keeping `.0`.
        // `{d:.1}` expanded `1e308` into its full 309-digit decimal value.
        Bson::Double(d) => secantus_core::format_double_spec(*d),
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
        // Rust's `Debug` is not mongod's rendering, and the catch-all reached
        // every type without an arm above: a regex read
        // `Regex { pattern: "a", options: "" }` where mongod says `/a/`, binary
        // read `Binary { subtype: Generic, bytes: [122] }` against
        // `BinData(0, 7A)`, and a timestamp `Timestamp { time: 1, increment: 1 }`
        // against `Timestamp(1, 1)`. Probed 8.2.11 (2026-09-02).
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        Bson::Binary(b) => format!(
            "BinData({}, {})",
            u8::from(b.subtype),
            b.bytes
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        ),
        Bson::Timestamp(t) => format!("Timestamp({}, {})", t.time, t.increment),
        Bson::JavaScriptCode(c) => c.clone(),
        Bson::JavaScriptCodeWithScope(c) => c.code.clone(),
        Bson::Symbol(sym) => format!("\"{sym}\""),
        Bson::MinKey => "MinKey".to_string(),
        Bson::MaxKey => "MaxKey".to_string(),
        other => format!("{other:?}"),
    }
}

/// Variables mongod defines for every pipeline. Listed rather than inferred, so
/// an unknown name is reported instead of silently accepted. `CLUSTER_TIME` /
/// `SEARCH_META` / `JS_SCOPE` are DEFINED but answer their own errors
/// (10071200 / 6347902 / 51144); they belong here so this check leaves them to
/// those paths. Mirrors `aggregate._SYSTEM_VARS`.
const SYSTEM_VARS: &[&str] = &[
    "ROOT",
    "CURRENT",
    "NOW",
    "REMOVE",
    "USER_ROLES",
    "CLUSTER_TIME",
    "SEARCH_META",
    "JS_SCOPE",
];

/// Stages whose whole spec is one expression.
const EXPR_SPEC_STAGES: &[&str] = &["$redact", "$replaceWith", "$sortByCount"];

/// Stages whose spec is `field: <expression>` pairs. The KEYS are field names.
const EXPR_MAP_STAGES: &[&str] = &["$project", "$addFields", "$set", "$group"];

/// Stages that wrap the error in `Invalid $<stage> :: caused by ::`; every other
/// stage reports it bare. Probed on 8.2.11.
const EXPR_WRAPPING_STAGES: &[&str] = &["$project", "$addFields", "$set"];

/// Operators whose argument must be a DOCUMENT, with mongod's code and wording.
/// Taken from mongod 8.2.11 one operator at a time -- the five phrasings are its
/// own and are not interchangeable ("found: {}" vs "found {}" vs no type at
/// all), which is why this is a table rather than one message with the operator
/// name substituted in. Mirrors `expressions._OBJECT_ARG`.
///
/// Like the arity check, this is a PARSE error: an empty collection reports it.
const OBJECT_ARG: &[(&str, i32, &str)] = &[
    (
        "$convert",
        9,
        "$convert expects an object of named arguments but found: {}",
    ),
    (
        "$dateAdd",
        5166400,
        "$dateAdd expects an object as its argument",
    ),
    (
        "$dateDiff",
        5166301,
        "$dateDiff only supports an object as its argument",
    ),
    (
        "$dateFromParts",
        40519,
        "$dateFromParts only supports an object as its argument",
    ),
    (
        "$dateFromString",
        40540,
        "$dateFromString only supports an object as an argument, found: {}",
    ),
    (
        "$dateSubtract",
        5166400,
        "$dateSubtract expects an object as its argument",
    ),
    (
        "$dateToParts",
        40524,
        "$dateToParts only supports an object as its argument",
    ),
    (
        "$dateToString",
        18629,
        "$dateToString only supports an object as its argument",
    ),
    (
        "$dateTrunc",
        5439007,
        "$dateTrunc only supports an object as its argument",
    ),
    (
        "$filter",
        28646,
        "$filter only supports an object as its argument",
    ),
    (
        "$let",
        16874,
        "$let only supports an object as its argument",
    ),
    (
        "$ltrim",
        50696,
        "$ltrim only supports an object as an argument, found {}",
    ),
    (
        "$map",
        16878,
        "$map only supports an object as its argument",
    ),
    (
        "$reduce",
        40075,
        "$reduce requires an object as an argument, found: {}",
    ),
    (
        "$regexFind",
        51103,
        "$regexFind expects an object of named arguments but found: {}",
    ),
    (
        "$regexFindAll",
        51103,
        "$regexFindAll expects an object of named arguments but found: {}",
    ),
    (
        "$regexMatch",
        51103,
        "$regexMatch expects an object of named arguments but found: {}",
    ),
    (
        "$replaceAll",
        51751,
        "$replaceAll requires an object as an argument, found: {}",
    ),
    (
        "$replaceOne",
        51751,
        "$replaceOne requires an object as an argument, found: {}",
    ),
    (
        "$rtrim",
        50696,
        "$rtrim only supports an object as an argument, found {}",
    ),
    (
        "$setField",
        4161100,
        "$setField only supports an object as its argument",
    ),
    (
        "$sortArray",
        2942500,
        "$sortArray requires an object as an argument, found: {}",
    ),
    (
        "$switch",
        40060,
        "$switch requires an object as an argument, found: {}",
    ),
    (
        "$trim",
        50696,
        "$trim only supports an object as an argument, found {}",
    ),
    (
        "$zip",
        34460,
        "$zip only supports an object as an argument, found {}",
    ),
];

/// mongod's error when a document-argument operator gets something else.
fn object_arg_problem(op: &str, arg: &Bson) -> Option<(i32, String)> {
    if matches!(arg, Bson::Document(_)) {
        return None;
    }
    let (_, code, template) = OBJECT_ARG.iter().find(|(name, _, _)| *name == op)?;
    Some((*code, template.replace("{}", bson_type_name(arg))))
}

/// mongod's 16020 when a fixed-arity operator gets the wrong argument count.
/// The table lives in the ENGINE (`secantus_core::expressions`), because the
/// evaluator needs it too: mongod unwraps a one-element array for the
/// single-argument operators, and getting that wrong produced silent wrong
/// values, not just wrong errors.
fn arity_problem(op: &str, arg: &Bson) -> Option<(i32, String)> {
    let want = secantus_core::expressions::fixed_arity(op)?;
    // `$cond`'s OBJECT form (`{if, then, else}`) is exempt: it is a document,
    // so it would otherwise count as 1 against an arity of 3.
    if op == "$cond" && matches!(arg, Bson::Document(_)) {
        return None;
    }
    let got = match arg {
        Bson::Array(a) => a.len(),
        _ => 1,
    };
    if got == want {
        return None;
    }
    // `$substr` is an ALIAS: mongod names the canonical operator.
    let name = if op == "$substr" { "$substrBytes" } else { op };
    // "1 arguments" is mongod's own plural, reproduced.
    Some((
        16020,
        format!("Expression {name} takes exactly {want} arguments. {got} were passed in."),
    ))
}

fn expression_problem(expr: &Bson, bound: &[String]) -> Option<(i32, String)> {
    match expr {
        Bson::String(s) => {
            let name = s.strip_prefix("$$")?;
            let base = name.split('.').next().unwrap_or(name);
            if base.is_empty() || bound.iter().any(|b| b == base) || SYSTEM_VARS.contains(&base) {
                return None;
            }
            Some((17276, format!("Use of undefined variable: {base}")))
        }
        Bson::Array(items) => items.iter().find_map(|i| expression_problem(i, bound)),
        Bson::Document(d) => {
            for (op, arg) in d {
                // Arity is STRUCTURAL, so it is checked even for `$literal`.
                if let Some(found) = arity_problem(op, arg).or_else(|| object_arg_problem(op, arg))
                {
                    return Some(found);
                }
                // `$literal`'s argument is DATA: `{$literal: "$$x"}` is the
                // string, and mongod does not resolve it.
                if op == "$literal" {
                    continue;
                }
                let found = match op.as_str() {
                    "$let" => problem_in_let(arg, bound),
                    "$map" | "$filter" => problem_in_binding(arg, bound, op),
                    "$reduce" => problem_in_reduce(arg, bound),
                    _ => expression_problem(arg, bound),
                };
                if found.is_some() {
                    return found;
                }
            }
            None
        }
        _ => None,
    }
}

/// `$let`: the bindings are evaluated in the OUTER scope — they cannot see each
/// other (probed) — and only `in` sees the new names.
fn problem_in_let(arg: &Bson, bound: &[String]) -> Option<(i32, String)> {
    let Bson::Document(d) = arg else { return None };
    let mut inner = bound.to_vec();
    if let Some(Bson::Document(vars)) = d.get("vars") {
        for (_, value) in vars {
            if let Some(found) = expression_problem(value, bound) {
                return Some(found);
            }
        }
        inner.extend(vars.keys().cloned());
    }
    d.get("in").and_then(|e| expression_problem(e, &inner))
}

/// `$map` / `$filter`: `input` is outer-scope; the body sees `as` (default
/// `this`).
fn problem_in_binding(arg: &Bson, bound: &[String], op: &str) -> Option<(i32, String)> {
    let Bson::Document(d) = arg else { return None };
    if let Some(found) = d.get("input").and_then(|e| expression_problem(e, bound)) {
        return Some(found);
    }
    let as_name = match d.get("as") {
        Some(Bson::String(s)) if !s.is_empty() => s.clone(),
        _ => "this".to_string(),
    };
    let mut inner = bound.to_vec();
    inner.push(as_name);
    let body = if op == "$filter" { "cond" } else { "in" };
    d.get(body).and_then(|e| expression_problem(e, &inner))
}

fn problem_in_reduce(arg: &Bson, bound: &[String]) -> Option<(i32, String)> {
    let Bson::Document(d) = arg else { return None };
    for key in ["input", "initialValue"] {
        if let Some(found) = d.get(key).and_then(|e| expression_problem(e, bound)) {
            return Some(found);
        }
    }
    let mut inner = bound.to_vec();
    inner.push("this".to_string());
    inner.push("value".to_string());
    d.get("in").and_then(|e| expression_problem(e, &inner))
}

/// The first `$$name` in a QUERY FILTER that names nothing.
///
/// A filter is query language, not an expression: `{s: "$$NOPE"}` matches the
/// literal string. Only `$expr` holds an expression, and only `$and` / `$or` /
/// `$nor` nest further filters. Everything else is left alone, the same
/// conservative rule [`undefined_variable_error`] follows — a false positive
/// would reject a VALID query.
///
/// This is the surface the pipeline walker did not cover: `find`, `count`,
/// `distinct`, `findAndModify` and the `q` of an `update` / `delete` all take a
/// filter, and an undefined variable in one answered the storage layer's
/// generic `BadValue` (2) `query uses a construct the Rust server does not
/// support` instead of mongod's 17276.
pub fn expression_problem_in_filter(filter: &Document, bound: &[String]) -> Option<(i32, String)> {
    for (key, value) in filter {
        match key.as_str() {
            "$expr" => {
                if let Some(found) = expression_problem(value, bound) {
                    return Some(found);
                }
            }
            "$and" | "$or" | "$nor" => {
                if let Bson::Array(subs) = value {
                    for sub in subs {
                        if let Bson::Document(d) = sub {
                            if let Some(found) = expression_problem_in_filter(d, bound) {
                                return Some(found);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The first `$$name` that names nothing in an UPDATE, whether it is an
/// operator document (no expressions, so nothing to find) or a PIPELINE.
/// Returns `(variable, stage)` like [`undefined_variable_error`].
pub fn expression_problem_in_update(
    update: &Bson,
    bound: &[String],
) -> Option<(i32, String, String)> {
    match update {
        Bson::Array(stages) => expression_problem_in_pipeline(stages, bound),
        _ => None,
    }
}

/// mongod's message for an undefined variable, with the stage wrapper it uses
/// inside `$project` / `$addFields` / `$set` and no wrapper anywhere else.
pub fn wrap_expression_problem(message: &str, stage: &str) -> String {
    if stage.is_empty() {
        message.to_string()
    } else {
        format!("Invalid {stage} :: caused by :: {message}")
    }
}

/// The first `$$name` in `pipeline` that names nothing, with the stage name for
/// mongod's wrapper (empty where mongod leaves the message bare).
///
/// mongod reports this at PARSE time — it fires on an EMPTY collection, where
/// nothing is ever evaluated — so this runs before the pipeline rather than
/// during it. That is also why the engine could not produce it: with no
/// documents there is no evaluation to fail.
///
/// Deliberately CONSERVATIVE. It descends only into positions known to be
/// expressions and ignores every stage it does not recognise: a false negative
/// leaves the previous behaviour, while a false positive would reject a VALID
/// pipeline — much worse than the generic `BadValue` this replaces. Mirrors
/// `aggregate.undefined_variable_in_pipeline`.
pub fn expression_problem_in_pipeline(
    pipeline: &[Bson],
    bound: &[String],
) -> Option<(i32, String, String)> {
    for stage in pipeline {
        let Bson::Document(stage) = stage else {
            continue;
        };
        if stage.len() != 1 {
            continue;
        }
        let Some((name, spec)) = stage.iter().next() else {
            continue;
        };
        let wrapper = if EXPR_WRAPPING_STAGES.contains(&name.as_str()) {
            name.clone()
        } else {
            String::new()
        };
        // `found` is a problem in THIS stage, which takes this stage's wrapper.
        // The nesting stages return their own fully-formed answer instead, since
        // the wrapper belongs to the INNER stage that failed.
        let found: Option<(i32, String)> = if EXPR_SPEC_STAGES.contains(&name.as_str()) {
            // `$redact` binds its three decision names for its own expression.
            let mut inner = bound.to_vec();
            if name == "$redact" {
                inner.extend([
                    "KEEP".to_string(),
                    "PRUNE".to_string(),
                    "DESCEND".to_string(),
                ]);
            }
            expression_problem(spec, &inner)
        } else if EXPR_MAP_STAGES.contains(&name.as_str()) {
            match spec {
                Bson::Document(d) => d.iter().find_map(|(_, v)| expression_problem(v, bound)),
                _ => None,
            }
        } else if name == "$replaceRoot" {
            match spec {
                Bson::Document(d) => d.get("newRoot").and_then(|e| expression_problem(e, bound)),
                _ => None,
            }
        } else if name == "$match" {
            // The filter is QUERY language, where `"$$x"` is a literal value to
            // match. Only `$expr` holds an expression, and it takes no wrapper.
            match spec {
                Bson::Document(d) => {
                    if let Some(p) = d.get("$expr").and_then(|e| expression_problem(e, bound)) {
                        return Some((p.0, p.1, String::new()));
                    }
                    None
                }
                _ => None,
            }
        } else if name == "$facet" {
            if let Bson::Document(d) = spec {
                for (_, sub) in d {
                    if let Bson::Array(p) = sub {
                        if let Some(found) = expression_problem_in_pipeline(p, bound) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        } else if name == "$lookup" {
            match spec {
                Bson::Document(d) => {
                    // `let` binds only inside this stage's own sub-pipeline:
                    // referencing it in a LATER stage is undefined (probed).
                    let mut inner = bound.to_vec();
                    let mut bad = None;
                    if let Some(Bson::Document(lv)) = d.get("let") {
                        for (_, value) in lv {
                            if let Some(found) = expression_problem(value, bound) {
                                bad = Some(found);
                                break;
                            }
                        }
                        inner.extend(lv.keys().cloned());
                    }
                    if bad.is_none() {
                        if let Some(Bson::Array(p)) = d.get("pipeline") {
                            if let Some(found) = expression_problem_in_pipeline(p, &inner) {
                                return Some(found);
                            }
                        }
                    }
                    bad
                }
                _ => None,
            }
        } else {
            None // every other stage is left alone on purpose
        };
        // An expression's ARITY and SPEC SHAPE are parse errors, checked before
        // anything is folded, which is why they carry the STAGE wrapper and not
        // the optimizer's. Mirrors `aggregate._expression_shape_problem`; the
        // Python server took the same fix.
        let found = found.or_else(|| expression_shape_problem(spec));
        if let Some((code, msg)) = found {
            return Some((code, msg, wrapper));
        }
    }
    None
}

/// `(low, high)` argument counts for the expressions mongod range-checks by
/// arity. A NON-array argument counts as one.
const EXPRESSION_ARITY: &[(&str, usize, usize)] = &[
    ("$indexOfArray", 2, 4),
    // `[value]` or `[value, place]` -- an empty or 3-element list is 28667 at
    // parse time, which used to reach the evaluator instead: `{$round: [3,1,2]}`
    // ANSWERED 3 and `{$round: []}` reported a type complaint (probed 8.2.11).
    ("$round", 1, 2),
    ("$trunc", 1, 2),
    ("$indexOfBytes", 2, 4),
    ("$indexOfCP", 2, 4),
    ("$range", 2, 3),
    ("$slice", 2, 3),
];

/// Operators mongod rejects at PARSE time for having too few operands, with the
/// code and the exact sentence each uses. Not one wording: `$ifNull` puts a
/// comma before "had" and `$setEquals` does not. A non-array operand counts as
/// one. Probed 8.2.11 (2026-09-02).
const MIN_OPERANDS: &[(&str, usize, i32, &str)] = &[
    (
        "$ifNull",
        2,
        1257300,
        "$ifNull needs at least two arguments, had: {}",
    ),
    (
        "$setEquals",
        2,
        17045,
        "$setEquals needs at least two arguments had: {}",
    ),
];

/// The `$convert` shorthands. Each takes exactly one operand, and mongod counts
/// it at PARSE time: an ARRAY of any length but one is `50723`, while a bare
/// operand is always a single argument whatever its type. Probed 8.2.11
/// (2026-09-02) -- `{$toInt: [1]}` is 1, `{$toInt: []}` is "got 0".
const SINGLE_ARG_CONVERSIONS: &[&str] = &[
    "$toBool",
    "$toDate",
    "$toDecimal",
    "$toDouble",
    "$toInt",
    "$toLong",
    "$toObjectId",
    "$toString",
];

/// The date extractors, which take a bare expression OR a one-element array.
const DATE_EXTRACTORS: &[&str] = &[
    "$dayOfMonth",
    "$dayOfWeek",
    "$dayOfYear",
    "$hour",
    "$isoDayOfWeek",
    "$isoWeek",
    "$isoWeekYear",
    "$millisecond",
    "$minute",
    "$month",
    "$second",
    "$week",
    "$year",
];

/// Operators whose spec document rejects an unrecognised key, with the code and
/// the WORDING each uses.
///
/// Two sentences, not one: a handful say "Unrecognized parameter to $op: k" and
/// the rest "$op found an unknown argument: k". Every code is its own -- they
/// share nothing, not even within a wording -- so this is a table rather than
/// one message with a name substituted. Operator names, codes and the accepted
/// argument list were all probed against 8.2.11 (2026-09-03) by feeding each
/// candidate key in turn and keeping the ones it did NOT reject.
///
/// `true` in the third slot selects the "Unrecognized parameter to" form.
/// Operator spec documents with a REQUIRED key. Missing it is a PARSE error, so
/// it takes the stage's wrapper. Codes measured individually against 8.2.11
/// (2026-09-05) -- they share nothing, not even within a family.
///
/// Checked AFTER the unknown-key tables above, because mongod reports an
/// unrecognised key before a missing required one: `{$firstN: {k: 1}}` is
/// "Unknown argument for 'n' operator: k" and only `{$firstN: {}}` is "Missing
/// value for 'n'". Getting that order wrong changes the CODE on shapes that are
/// already right -- it did exactly that on the Python side before the order was
/// corrected (`expressions._expression_shape_problem`).
const REQUIRED_SPEC_KEY: &[(&str, &str, i32, &str)] = &[
    (
        "$convert",
        "input",
        9,
        "Missing 'input' parameter to $convert",
    ),
    (
        "$dateDiff",
        "startDate",
        5166303,
        "Missing 'startDate' parameter to $dateDiff",
    ),
    ("$firstN", "n", 5787906, "Missing value for 'n'"),
    ("$lastN", "n", 5787906, "Missing value for 'n'"),
    ("$maxN", "n", 5787906, "Missing value for 'n'"),
    ("$minN", "n", 5787906, "Missing value for 'n'"),
];

const UNKNOWN_ARGUMENT: &[(&str, i32, bool, &[&str])] = &[
    ("$cond", 17083, true, &["if", "then", "else"]),
    ("$filter", 28647, true, &["input", "as", "cond", "limit"]),
    ("$let", 16875, true, &["vars", "in"]),
    ("$map", 16879, true, &["input", "as", "in"]),
    (
        "$convert",
        9,
        false,
        &["input", "to", "onError", "onNull", "format", "byteOrder"],
    ),
    ("$ltrim", 50694, false, &["input", "chars"]),
    ("$rtrim", 50694, false, &["input", "chars"]),
    ("$trim", 50694, false, &["input", "chars"]),
    ("$reduce", 40076, false, &["input", "initialValue", "in"]),
    ("$regexFind", 31024, false, &["input", "regex", "options"]),
    (
        "$regexFindAll",
        31024,
        false,
        &["input", "regex", "options"],
    ),
    ("$regexMatch", 31024, false, &["input", "regex", "options"]),
    (
        "$replaceAll",
        51750,
        false,
        &["input", "find", "replacement"],
    ),
    (
        "$replaceOne",
        51750,
        false,
        &["input", "find", "replacement"],
    ),
    ("$setField", 4161101, false, &["field", "input", "value"]),
    ("$sortArray", 2942501, false, &["input", "sortBy"]),
    ("$switch", 40067, false, &["branches", "default"]),
    (
        "$zip",
        34464,
        false,
        &["inputs", "useLongestLength", "defaults"],
    ),
];

/// The `n`-operator family, whose spec document takes only known arguments.
///
/// mongod checks the UNKNOWN argument before it checks for a missing `n`, so
/// `{$firstN: {k: 1}}` is "Unknown argument" rather than "Missing value for
/// 'n'". The `$median` / `$percentile` pair belongs to a different family again
/// and reports the IDL's own wording. Probed 8.2.11 (2026-09-03).
const N_OPERATOR_ARGUMENTS: &[(&str, &[&str])] = &[
    ("$firstN", &["input", "n"]),
    ("$lastN", &["input", "n"]),
    ("$minN", &["input", "n"]),
    ("$maxN", &["input", "n"]),
];

/// `$median` / `$percentile`: an unrecognised key is the IDL's generic
/// unknown-field complaint, naming the operator and the key.
const IDL_UNKNOWN_FIELD: &[(&str, &[&str])] = &[
    ("$median", &["input", "method"]),
    ("$percentile", &["input", "p", "method"]),
];

/// Accumulator-style expressions whose spec must be a document. Each carries its
/// OWN Location code -- probed 8.2.11 (2026-09-02), they do not share one.
const OBJECT_SPEC_EXPRESSIONS: &[(&str, i32)] = &[
    ("$firstN", 5787801),
    ("$lastN", 5787801),
    ("$minN", 5787900),
    ("$maxN", 5787900),
    ("$median", 7436201),
    ("$percentile", 7436200),
    ("$topN", 168),
    ("$bottomN", 168),
];

/// An unrecognised argument inside a date-operator spec: the operator's known
/// arguments, its code, and the tail three of them append.
const DATE_SPEC_ARGUMENTS: &[(&str, &[&str], i32, &str)] = &[
    (
        "$dateAdd",
        &["startDate", "unit", "amount", "timezone"],
        5166401,
        ". Expected arguments are startDate, unit, amount, and optionally timezone.",
    ),
    (
        "$dateSubtract",
        &["startDate", "unit", "amount", "timezone"],
        5166401,
        ". Expected arguments are startDate, unit, amount, and optionally timezone.",
    ),
    (
        "$dateDiff",
        &["startDate", "endDate", "unit", "timezone", "startOfWeek"],
        5166302,
        "",
    ),
    (
        "$dateFromParts",
        &[
            "year",
            "isoWeekYear",
            "month",
            "isoWeek",
            "day",
            "isoDayOfWeek",
            "hour",
            "minute",
            "second",
            "millisecond",
            "timezone",
        ],
        40518,
        "",
    ),
    ("$dateToParts", &["date", "timezone", "iso8601"], 40520, ""),
    (
        "$dateFromString",
        &["dateString", "format", "timezone", "onError", "onNull"],
        40541,
        "",
    ),
    (
        "$dateToString",
        &["date", "format", "timezone", "onNull"],
        18534,
        "",
    ),
    (
        "$dateTrunc",
        &["date", "unit", "binSize", "timezone", "startOfWeek"],
        5439008,
        ". Expected arguments are date, unit, and optionally, binSize, timezone, startOfWeek",
    ),
];

/// The first arity / spec-shape error in an expression, as mongod PARSES it.
///
/// These are raised while building the expression tree, before anything folds,
/// so the caller gives them the stage's wrapper. Several of them were answered
/// `ok` here -- a spec with an unrecognised date argument simply ignored it.
pub(crate) fn expression_shape_problem(spec: &Bson) -> Option<(i32, String)> {
    match spec {
        Bson::Document(d) => {
            for (key, value) in d {
                if let Some((_, low, high)) = EXPRESSION_ARITY.iter().find(|(op, _, _)| *op == key)
                {
                    let count = match value {
                        Bson::Array(a) => a.len(),
                        _ => 1,
                    };
                    if count < *low || count > *high {
                        return Some((
                            28667,
                            format!(
                                "Expression {key} takes at least {low} arguments, and at most {high}, but {count} were passed in."
                            ),
                        ));
                    }
                }
                if let Some((_, min, code, template)) =
                    MIN_OPERANDS.iter().find(|(op, _, _, _)| *op == key)
                {
                    let count = match value {
                        Bson::Array(a) => a.len(),
                        _ => 1,
                    };
                    if count < *min {
                        return Some((*code, template.replace("{}", &count.to_string())));
                    }
                }
                // `$rand` takes an empty document or an empty array and nothing
                // else: a scalar is 10065 (a PARAMETER complaint) while a
                // non-empty array is 3040501 (an ARGUMENT one). Two codes for
                // what reads as one mistake -- probed 8.2.11 (2026-09-02).
                if key == "$rand" {
                    match value {
                        // An EMPTY document or array is the no-argument call.
                        Bson::Document(d) if d.is_empty() => {}
                        Bson::Array(a) if a.is_empty() => {}
                        // A non-empty one of either is "does not currently
                        // accept arguments"; a SCALAR is the different
                        // complaint that it is not an object at all. Probed
                        // 8.2.11 (2026-09-02) -- `{$rand: {k: 1}}` is 3040501,
                        // not the 10065 a document-shaped check would give.
                        Bson::Document(_) | Bson::Array(_) => {
                            return Some((
                                3040501,
                                "$rand does not currently accept arguments".to_string(),
                            ))
                        }
                        _ => {
                            return Some((
                                10065,
                                "invalid parameter: expected an object ($rand)".to_string(),
                            ))
                        }
                    }
                }
                if key == "$getField" {
                    if let Bson::Document(spec) = value {
                        // A single `$`-key document is a nested EXPRESSION, not
                        // the options form: `{$getField: {$literal: "$odd"}}` is
                        // how a literally-dollared field name is written, and
                        // reading it as options refused `$literal` as an unknown
                        // argument (probed 8.2.11, 2026-09-02).
                        let is_operator = spec.len() == 1
                            && spec.keys().next().is_some_and(|k| k.starts_with('$'));
                        if let Some(bad) = spec
                            .keys()
                            .filter(|_| !is_operator)
                            .find(|k| !matches!(k.as_str(), "field" | "input"))
                        {
                            return Some((
                                3041701,
                                format!("$getField found an unknown argument: {bad}"),
                            ));
                        }
                        // Also parse-time: both of `$getField`'s object-form
                        // complaints fire on an EMPTY collection. The bare
                        // form's "must evaluate to type String" does not --
                        // that one runs per document, so it stays in the
                        // evaluator (probed 8.2.11, 2026-09-02).
                        if !is_operator && !spec.contains_key("input") {
                            return Some((
                                3041703,
                                "$getField requires 'input' to be specified".to_string(),
                            ));
                        }
                    }
                }
                if SINGLE_ARG_CONVERSIONS.contains(&key.as_str()) {
                    if let Bson::Array(a) = value {
                        if a.len() != 1 {
                            return Some((
                                50723,
                                format!("{key} requires a single argument, got {}", a.len()),
                            ));
                        }
                    }
                }
                if DATE_EXTRACTORS.contains(&key.as_str()) {
                    // A document operand is the `{date, timezone}` OPTIONS
                    // form -- unless it is a nested operator expression
                    // (`{$year: {$add: [1, 2]}}`), which is one `$`-key.
                    if let Bson::Document(opts) = value {
                        let is_operator = opts.len() == 1
                            && opts.keys().next().is_some_and(|k| k.starts_with('$'));
                        if !is_operator {
                            // The unrecognised-key check runs FIRST and reports
                            // the first offender, even when `date` is present
                            // and valid; only then does the missing-`date`
                            // check fire. Probed 8.2.11 (2026-09-02).
                            if let Some(bad) = opts
                                .keys()
                                .find(|k| !matches!(k.as_str(), "date" | "timezone"))
                            {
                                return Some((
                                    40535,
                                    format!("unrecognized option to {key}: \"{bad}\""),
                                ));
                            }
                            if !opts.contains_key("date") {
                                return Some((
                                    40539,
                                    format!(
                                        "missing 'date' argument to {key}, provided: {key}: {}",
                                        render_stage_value(value)
                                    ),
                                ));
                            }
                        }
                    }
                    if let Bson::Array(a) = value {
                        if a.len() != 1 {
                            return Some((
                                40536,
                                format!(
                                    "{key} accepts exactly one argument if given an array, but was given {}",
                                    a.len()
                                ),
                            ));
                        }
                    }
                }
                if let Some((_, fields, code, tail)) =
                    DATE_SPEC_ARGUMENTS.iter().find(|(op, _, _, _)| *op == key)
                {
                    if let Bson::Document(inner) = value {
                        for field in inner.keys() {
                            if !fields.contains(&field.as_str()) {
                                return Some((
                                    *code,
                                    format!("Unrecognized argument to {key}: {field}{tail}"),
                                ));
                            }
                        }
                    }
                }
                if let Some((_, code, is_parameter, known)) =
                    UNKNOWN_ARGUMENT.iter().find(|(op, _, _, _)| *op == key)
                {
                    if let Bson::Document(spec) = value {
                        if let Some(bad) = spec.keys().find(|k| !known.contains(&k.as_str())) {
                            let message = if *is_parameter {
                                format!("Unrecognized parameter to {key}: {bad}")
                            } else {
                                format!("{key} found an unknown argument: {bad}")
                            };
                            return Some((*code, message));
                        }
                    }
                }
                if let Some((_, known)) = N_OPERATOR_ARGUMENTS.iter().find(|(op, _)| *op == key) {
                    if let Bson::Document(spec) = value {
                        if let Some(bad) = spec.keys().find(|k| !known.contains(&k.as_str())) {
                            return Some((
                                5787901,
                                format!("Unknown argument for 'n' operator: {bad}"),
                            ));
                        }
                    }
                }
                if let Some((_, known)) = IDL_UNKNOWN_FIELD.iter().find(|(op, _)| *op == key) {
                    if let Bson::Document(spec) = value {
                        if let Some(bad) = spec.keys().find(|k| !known.contains(&k.as_str())) {
                            return Some((
                                40415,
                                format!("BSON field '{key}.{bad}' is an unknown field."),
                            ));
                        }
                    }
                }
                if let Some((_, code)) = OBJECT_SPEC_EXPRESSIONS.iter().find(|(op, _)| *op == key) {
                    if !matches!(value, Bson::Document(_)) {
                        return Some((
                            *code,
                            format!(
                                "specification must be an object; found {key}: {}",
                                render_stage_value(value)
                            ),
                        ));
                    }
                }
                // Required keys LAST: see `REQUIRED_SPEC_KEY`. Every
                // unknown-key table above has already had its say.
                if let Some((_, needed, code, message)) =
                    REQUIRED_SPEC_KEY.iter().find(|(op, _, _, _)| *op == key)
                {
                    if let Bson::Document(spec) = value {
                        if !spec.contains_key(needed) {
                            return Some((*code, (*message).to_string()));
                        }
                    }
                }
                if key == "$dateFromParts" {
                    if let Bson::Document(spec) = value {
                        if !spec.contains_key("year") && !spec.contains_key("isoWeekYear") {
                            return Some((
                                40516,
                                "$dateFromParts requires either 'year' or 'isoWeekYear' \
                                 to be present"
                                    .to_string(),
                            ));
                        }
                    }
                }
                if let Some(found) = expression_shape_problem(value) {
                    return Some(found);
                }
            }
            None
        }
        Bson::Array(a) => a.iter().find_map(expression_shape_problem),
        _ => None,
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
                if n.is_nan() {
                    // mongod names NaN rather than calling it a non-integer,
                    // and renders it lower-case -- the same shape the rest of
                    // the integer-argument family uses.
                    return Some((
                        code,
                        format!(
                            "invalid argument to {name} stage: Expected an integer, \
                             but found NaN in: {name}: nan"
                        ),
                    ));
                }
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
                Bson::Document(d) => {
                    // An UNKNOWN option is reported before the missing `path`,
                    // so `{$unwind: {a: 1}}` names `a` (probed 8.2.11).
                    const KNOWN: &[&str] =
                        &["path", "includeArrayIndex", "preserveNullAndEmptyArrays"];
                    if let Some(field) = d.keys().find(|k| !KNOWN.contains(&k.as_str())) {
                        Some((
                            28811,
                            format!("unrecognized option to $unwind stage: {field}"),
                        ))
                    } else if !d.contains_key("path") {
                        Some((28812, "no path specified to $unwind stage".to_string()))
                    } else {
                        None
                    }
                }
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
            // `$set` names ITSELF, not `$addFields`. The two share an
            // implementation and the message was hard-coded to the alias the
            // implementation happened to be named after -- so a `$set` user saw
            // an error about a stage they had not written (probed 8.2.11).
            "$addFields" | "$set" if !matches!(spec, Bson::Document(_)) => Some((
                40272,
                format!(
                    "{name} specification stage must be an object, got {}",
                    bson_type_name(spec)
                ),
            )),
            "$project" if !matches!(spec, Bson::Document(_)) => Some((
                15969,
                "$project specification must be an object".to_string(),
            )),
            // Each of these deferred, and a defer on the standalone server is
            // "the STAGE is unsupported" -- for what is really a bad argument to
            // a supported one. Every code and wording probed on 8.2.11; they do
            // not share a shape, so they are listed rather than generated.
            "$out" if !matches!(spec, Bson::String(_) | Bson::Document(_)) => Some((
                16990,
                format!(
                    "$out only supports a string or object argument, but found {}",
                    bson_type_name(spec)
                ),
            )),
            "$merge" if !matches!(spec, Bson::String(_) | Bson::Document(_)) => Some((
                14,
                format!(
                    "$merge requires a string or object argument, but found {}",
                    bson_type_name(spec)
                ),
            )),
            "$unset" if !matches!(spec, Bson::String(_) | Bson::Array(_)) => Some((
                31002,
                "$unset specification must be a string or an array".to_string(),
            )),
            // An EMPTY path is not a field path. This used to be accepted and
            // unset nothing, reporting success.
            "$unset" if matches!(spec, Bson::String(s) if s.is_empty()) => Some((
                40352,
                "FieldPath cannot be constructed with empty string".to_string(),
            )),
            "$bucketAuto" if !matches!(spec, Bson::Document(_)) => Some((
                40240,
                format!(
                    "The argument to $bucketAuto must be an object, but found type: {}",
                    bson_type_name(spec)
                ),
            )),
            // --- spec-CONTENT validation (2026-09-02) -------------------------
            // These all deferred, which on the standalone server reads as "the
            // stage is unsupported". mongod parses a spec field by field, so an
            // UNKNOWN or specifically-missing field is reported before the
            // generic "requires X and Y" -- the same rule the Python server
            // just adopted, with each stage's own code and wording.
            "$project" if matches!(spec, Bson::Document(d) if d.is_empty()) => Some((
                51272,
                "projection specification must have at least one field".to_string(),
            )),
            "$sort" if matches!(spec, Bson::Document(d) if d.is_empty()) => Some((
                15976,
                "$sort stage must have at least one sort key".to_string(),
            )),
            "$count" if matches!(spec, Bson::String(s) if s.is_empty()) => Some((
                40157,
                "the count field must be a non-empty string".to_string(),
            )),
            "$unset" if matches!(spec, Bson::Array(a) if a.is_empty()) => Some((
                31119,
                "$unset specification must be a string or an array with at least one field"
                    .to_string(),
            )),
            "$unset" if matches!(spec, Bson::Array(a) if a.iter().any(|e| !matches!(e, Bson::String(_)))) => {
                Some((
                    31120,
                    "$unset specification must be a string or an array containing only \
                     string values"
                        .to_string(),
                ))
            }
            "$group" => stage_group_problem(spec),
            "$sortByCount" if matches!(spec, Bson::Document(_)) => Some((
                40147,
                "the sortByCount field must be defined as a $-prefixed path or an \
                 expression inside an object"
                    .to_string(),
            )),
            "$geoNear" if matches!(spec, Bson::Document(d) if !d.contains_key("near")) => {
                Some((5860400, "$geoNear requires a 'near' argument".to_string()))
            }
            // An ARRAY is a document in BSON, so mongod accepts it as the spec
            // and then reports the missing `near`; a scalar is the type error.
            "$geoNear" if matches!(spec, Bson::Array(_)) => {
                Some((5860400, "$geoNear requires a 'near' argument".to_string()))
            }
            "$lookup"
                if matches!(spec, Bson::Document(d)
                    if !d.contains_key("from") && !d.contains_key("pipeline")) =>
            {
                Some((
                    9,
                    "must specify 'pipeline' when 'from' is empty".to_string(),
                ))
            }
            "$graphLookup" if matches!(spec, Bson::Document(d) if !d.contains_key("from")) => {
                let rendered = render_spec_spaced(spec);
                Some((
                    9,
                    format!(
                        "missing 'from' option to $graphLookup stage specification: {rendered}"
                    ),
                ))
            }
            "$replaceRoot" | "$densify" | "$fill" | "$unionWith" | "$out" | "$merge"
                if matches!(spec, Bson::Document(_)) =>
            {
                stage_field_problem(name, spec)
            }
            "$sample" if matches!(spec, Bson::Document(_)) => stage_field_problem(name, spec),
            "$lookup" | "$graphLookup" if matches!(spec, Bson::Document(_)) => {
                stage_field_problem(name, spec)
            }
            "$bucket" | "$bucketAuto" if matches!(spec, Bson::Document(_)) => {
                stage_field_problem(name, spec)
            }
            "$replaceRoot" if !matches!(spec, Bson::Document(_)) => Some((
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
            // NOT 40229: `$replaceWith` takes an EXPRESSION, not a spec
            // document, so a scalar is evaluated and reported as a non-object
            // RESULT (40228) -- probed 8.2.11. This asserted `$replaceRoot`'s
            // code because the two shared one arm.
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
        assert!(err_for("$densify", Bson::Int32(5))
            .1
            .starts_with("The $densify"));
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
        assert_eq!(
            err_for("$sortByCount", Bson::String("nodollar".into())).0,
            40148
        );
        assert_eq!(
            err_for("$sortByCount", Bson::Document(Document::new())).0,
            40147
        );
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
        let err =
            require_required_string(&doc! {"collection": 5}, "collection", "getMore.collection")
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
        assert!(
            err.errmsg.ends_with("expected types '[string]'"),
            "{}",
            err.errmsg
        );
        let err = require_index_name_or_key(&doc! {"index": 5}, "index", "dropIndexes.index")
            .unwrap_err();
        assert!(
            err.errmsg.ends_with("expected types '[string, object]'"),
            "{}",
            err.errmsg
        );
    }

    #[test]
    fn index_spec_bool_and_ttl_carry_mongods_broken_quoting() {
        // mongod never closes the quote it opens before the field name. This
        // asserts the defect on purpose: fidelity is the point.
        let spec = doc! {"key": {"a": 1}, "name": "i", "unique": "x"};
        let err = require_index_spec_bool(&spec, "unique").unwrap_err();
        assert!(
            err.errmsg.contains(
                "The field 'unique has value unique: \"x\", which is not convertible to bool"
            ),
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
        // An identifier nested inside `$and` is named by that filter too — a
        // top-level-keys-only read called this valid filter missing.
        let nested = vec![doc! {"$and": [{"e.g": {"$gt": 8}}]}];
        assert!(array_filter_identifier_error(&update, &nested).is_none());
    }
}

#[cfg(test)]
mod undefined_var_tests {
    use super::*;
    use bson::doc;

    /// `(variable-or-message, stage)` for the first problem, so the existing
    /// assertions read unchanged.
    fn find(pipeline: Vec<Bson>) -> Option<(String, String)> {
        expression_problem_in_pipeline(&pipeline, &[])
            .map(|(_, msg, stage)| (msg.rsplit(": ").next().unwrap_or(&msg).to_string(), stage))
    }

    fn stage(name: &str, spec: Bson) -> Bson {
        let mut d = Document::new();
        d.insert(name, spec);
        Bson::Document(d)
    }

    #[test]
    fn an_undefined_variable_is_found_with_its_stage_wrapper() {
        // Only the field-assignment stages carry a wrapper; the rest are bare.
        assert_eq!(
            find(vec![stage(
                "$project",
                Bson::Document(doc! {"x": "$$NOPE"})
            )]),
            Some(("NOPE".into(), "$project".into()))
        );
        assert_eq!(
            find(vec![stage(
                "$group",
                Bson::Document(doc! {"_id": "$$NOPE"})
            )]),
            Some(("NOPE".into(), String::new()))
        );
        assert_eq!(
            find(vec![stage("$redact", Bson::String("$$NOPE".into()))]),
            Some(("NOPE".into(), String::new()))
        );
    }

    #[test]
    fn it_descends_into_nested_documents_and_arrays() {
        assert!(find(vec![stage(
            "$addFields",
            Bson::Document(doc! {"x": {"y": "$$NOPE"}})
        )])
        .is_some());
        assert!(find(vec![stage(
            "$addFields",
            Bson::Document(doc! {"x": [1, "$$NOPE"]})
        )])
        .is_some());
        assert!(find(vec![stage(
            "$facet",
            Bson::Document(doc! {"f": [{"$project": {"x": "$$NOPE"}}]})
        )])
        .is_some());
    }

    /// The false-positive guards. A checker that flags any of these rejects a
    /// VALID pipeline, which is worse than the generic error it replaces.
    #[test]
    fn valid_pipelines_are_left_alone() {
        // `$match`'s filter is query language: `"$$NOPE"` is a value to match.
        assert!(find(vec![stage("$match", Bson::Document(doc! {"s": "$$NOPE"}))]).is_none());
        // `$literal`'s argument is data.
        assert!(find(vec![stage(
            "$project",
            Bson::Document(doc! {"x": {"$literal": "$$NOPE"}})
        )])
        .is_none());
        // Every binding form defines its name.
        for spec in [
            doc! {"x": {"$let": {"vars": {"v": 1}, "in": "$$v"}}},
            doc! {"x": {"$map": {"input": [1], "as": "m", "in": "$$m"}}},
            doc! {"x": {"$map": {"input": [1], "in": "$$this"}}},
            doc! {"x": {"$filter": {"input": [1], "cond": {"$gt": ["$$this", 1]}}}},
            doc! {"x": {"$reduce": {"input": [1], "initialValue": 0,
            "in": {"$add": ["$$value", "$$this"]}}}},
            doc! {"x": "$$ROOT"},
            doc! {"x": "$$NOW"},
            doc! {"x": "$$REMOVE"},
        ] {
            assert!(
                find(vec![stage("$project", Bson::Document(spec.clone()))]).is_none(),
                "{spec:?}"
            );
        }
        // `$redact` binds its three decision names for its own expression.
        assert!(find(vec![stage(
            "$redact",
            Bson::Document(doc! {"$cond": [true, "$$KEEP", "$$PRUNE"]})
        )])
        .is_none());
    }

    #[test]
    fn a_binding_does_not_escape_its_scope() {
        // `$let`'s name is gone after `in` ...
        assert_eq!(
            find(vec![stage(
                "$project",
                Bson::Document(doc! {"y": {"$let": {"vars": {"v": 1}, "in": "$$v"}}, "z": "$$v"})
            )]),
            Some(("v".into(), "$project".into()))
        );
        // ... and its bindings cannot see each other.
        assert_eq!(
            find(vec![stage(
                "$project",
                Bson::Document(doc! {"x": {"$let": {"vars": {"a1": 1, "b1": "$$a1"},
                "in": "$$b1"}}})
            )]),
            Some(("a1".into(), "$project".into()))
        );
    }

    #[test]
    fn lookup_let_binds_only_inside_its_own_pipeline() {
        let lookup = doc! {"from": "other", "let": {"lv": "$a"},
        "pipeline": [{"$match": {"$expr": {"$eq": ["$$lv", 1]}}}], "as": "r"};
        assert!(find(vec![stage("$lookup", Bson::Document(lookup.clone()))]).is_none());
        // Referenced after the stage, it is undefined.
        assert_eq!(
            find(vec![
                stage("$lookup", Bson::Document(lookup)),
                stage("$project", Bson::Document(doc! {"x": "$$lv"})),
            ]),
            Some(("lv".into(), "$project".into()))
        );
    }

    #[test]
    fn command_level_let_names_are_bound() {
        let pipeline = vec![stage("$project", Bson::Document(doc! {"x": "$$cv"}))];
        assert!(expression_problem_in_pipeline(&pipeline, &["cv".to_string()]).is_none());
        assert!(expression_problem_in_pipeline(&pipeline, &[]).is_some());
    }

    /// The arity table is derived from mongod; these pin the rules that are not
    /// "count the list": the object form, the alias, and the parse-time code.
    #[test]
    fn fixed_arity_is_checked_with_mongods_own_wording() {
        let p = |e: Bson| {
            expression_problem_in_pipeline(
                &[stage("$addFields", Bson::Document(doc! {"z": e}))],
                &[],
            )
        };
        let (code, msg, stage_name) = p(bson::bson!({"$abs": [1, 2]})).unwrap();
        assert_eq!(code, 16020);
        assert_eq!(
            msg,
            "Expression $abs takes exactly 1 arguments. 2 were passed in."
        );
        assert_eq!(stage_name, "$addFields");
        // A bare argument, a one-element list and a nested expression are all
        // ONE argument.
        assert!(p(bson::bson!({"$abs": 5})).is_none());
        assert!(p(bson::bson!({"$abs": [5]})).is_none());
        assert!(p(bson::bson!({"$abs": {"$add": [1, 2]}})).is_none());
        // `$cond`'s object form carries all three arguments and is exempt.
        assert!(p(bson::bson!({"$cond": {"if": true, "then": 1, "else": 2}})).is_none());
        assert_eq!(p(bson::bson!({"$cond": [true, 1]})).unwrap().0, 16020);
        // `$substr` is reported under its canonical name.
        assert!(p(bson::bson!({"$substr": 1}))
            .unwrap()
            .1
            .starts_with("Expression $substrBytes"));
    }

    #[test]
    fn a_one_element_list_is_one_argument_for_the_engine_too() {
        // The unwrap lives in the engine, so the two agree on what `[x]` means.
        assert_eq!(secantus_core::expressions::fixed_arity("$abs"), Some(1));
        assert_eq!(secantus_core::expressions::fixed_arity("$eq"), Some(2));
        assert_eq!(secantus_core::expressions::fixed_arity("$cond"), Some(3));
        assert_eq!(secantus_core::expressions::fixed_arity("$add"), None);
    }
}

/// mongod's document rendering, spaced inside the braces -- what
/// `$graphLookup`'s missing-`from` message echoes, unlike the compact value
/// form used elsewhere.
fn render_spec_spaced(spec: &Bson) -> String {
    match spec {
        Bson::Document(d) if d.is_empty() => "{}".to_string(),
        Bson::Document(d) => format!(
            "{{ {} }}",
            d.iter()
                .map(|(k, v)| format!("{k}: {}", render_stage_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => render_stage_value(other),
    }
}

/// A non-accumulator field outranks the missing `_id`, so `{$group: {a: 1}}`
/// names `a` rather than asking for an `_id`.
fn stage_group_problem(spec: &Bson) -> Option<(i32, String)> {
    let Bson::Document(d) = spec else { return None };
    for (field, value) in d {
        if field != "_id" && !matches!(value, Bson::Document(_)) {
            return Some((
                40234,
                format!("The field '{field}' must be an accumulator object"),
            ));
        }
    }
    if !d.contains_key("_id") {
        return Some((
            15955,
            "a group specification must include an _id".to_string(),
        ));
    }
    None
}

/// The unknown-field / missing-required-field pair, per stage. Each stage has
/// its own code and wording for the unknown half; the missing half is the IDL's
/// 40414 everywhere except `$sample`. Probed 8.2.11 (2026-09-02).
fn stage_field_problem(name: &str, spec: &Bson) -> Option<(i32, String)> {
    let Bson::Document(d) = spec else { return None };
    let (known, required): (&[&str], Option<&str>) = match name {
        "$replaceRoot" => (&["newRoot"], Some("newRoot")),
        "$densify" => (&["field", "range", "partitionByFields"], Some("field")),
        "$fill" => (
            &["output", "partitionBy", "partitionByFields", "sortBy"],
            Some("output"),
        ),
        "$unionWith" => (&["coll", "pipeline"], None),
        "$sample" => (&["size"], None),
        "$bucket" => (&["groupBy", "boundaries", "default", "output"], None),
        "$bucketAuto" => (&["groupBy", "buckets", "output", "granularity"], None),
        "$out" => (&["coll", "db", "timeseries"], Some("coll")),
        // Reached only once `from` is present -- the missing-`from` arms above
        // outrank both of these.
        "$lookup" => (
            &[
                "from",
                "localField",
                "foreignField",
                "as",
                "let",
                "pipeline",
            ],
            Some("as"),
        ),
        "$graphLookup" => (
            &[
                "from",
                "startWith",
                "connectFromField",
                "connectToField",
                "as",
                "maxDepth",
                "depthField",
                "restrictSearchWithMatch",
            ],
            None,
        ),
        "$merge" => (
            &["into", "on", "let", "whenMatched", "whenNotMatched"],
            Some("into"),
        ),
        _ => return None,
    };
    for field in d.keys() {
        if known.contains(&field.as_str()) {
            continue;
        }
        return Some(match name {
            "$graphLookup" => (40104, format!("Unknown argument to $graphLookup: {field}")),
            "$sample" => (28748, format!("unrecognized option to $sample: {field}")),
            "$bucket" => (40197, format!("Unrecognized option to $bucket: {field}.")),
            "$bucketAuto" => (
                40245,
                format!("Unrecognized option to $bucketAuto: {field}"),
            ),
            _ => (
                40415,
                format!("BSON field '{name}.{field}' is an unknown field."),
            ),
        });
    }
    if name == "$unionWith" && !d.contains_key("coll") {
        // A spec with no `coll` is legal only when the pipeline supplies its own
        // documents; mongod says so rather than "requires 'coll'". Checked here
        // rather than in a separate match arm because Rust arms do not fall
        // through -- the arm that dispatches here would have swallowed it.
        return Some((
            9,
            "$unionWith stage without explicit collection must have a pipeline \
             with $documents as first stage"
                .to_string(),
        ));
    }
    if name == "$sample" && !d.contains_key("size") {
        return Some((28749, "$sample stage must specify a size".to_string()));
    }
    if name == "$bucket" && !(d.contains_key("groupBy") && d.contains_key("boundaries")) {
        return Some((
            40198,
            "$bucket requires 'groupBy' and 'boundaries' to be specified.".to_string(),
        ));
    }
    if name == "$bucketAuto" && !(d.contains_key("groupBy") && d.contains_key("buckets")) {
        return Some((
            40246,
            "$bucketAuto requires 'groupBy' and 'buckets' to be specified".to_string(),
        ));
    }
    if name == "$graphLookup"
        && ![
            "from",
            "as",
            "startWith",
            "connectFromField",
            "connectToField",
        ]
        .iter()
        .all(|f| d.contains_key(*f))
    {
        return Some((
            40105,
            "$graphLookup requires 'from', 'as', 'startWith', 'connectFromField', \
             and 'connectToField' to be specified."
                .to_string(),
        ));
    }
    if let Some(field) = required {
        if !d.contains_key(field) {
            return Some((
                40414,
                format!("BSON field '{name}.{field}' is missing but a required field"),
            ));
        }
    }
    None
}

#[cfg(test)]
mod stage_validation_order_tests {
    //! mongod checks a stage spec field by field, so ORDER decides the message.
    //!
    //! Every value here was probed against mongod 8.2.11 (2026-09-02) via
    //! `tools/probes/aggregation_stage_specs.py`, which went from 219 divergent
    //! shapes on this server to 0. Before that the probe had no Rust column at
    //! all, so none of this surface had ever been compared.

    use super::*;
    use bson::Bson;

    fn err(stage: &str, spec: Bson) -> (i32, String) {
        let mut d = bson::Document::new();
        d.insert(stage, spec);
        stage_spec_error(&[Bson::Document(d)]).expect("expected an error")
    }

    fn ok(stage: &str, spec: Bson) {
        let mut d = bson::Document::new();
        d.insert(stage, spec);
        assert_eq!(stage_spec_error(&[Bson::Document(d)]), None);
    }

    fn doc_of(pairs: &[(&str, Bson)]) -> Bson {
        let mut d = bson::Document::new();
        for (k, v) in pairs {
            d.insert(*k, v.clone());
        }
        Bson::Document(d)
    }

    #[test]
    fn an_unknown_field_beats_the_generic_requires_message() {
        // Each stage has its OWN code and wording -- `$bucket` ends in a period
        // and `$bucketAuto` does not.
        let one = doc_of(&[("a", Bson::Int32(1))]);
        assert_eq!(
            err("$replaceRoot", one.clone()),
            (
                40415,
                "BSON field '$replaceRoot.a' is an unknown field.".to_string()
            )
        );
        assert_eq!(
            err("$sample", one.clone()),
            (28748, "unrecognized option to $sample: a".to_string())
        );
        assert_eq!(
            err("$bucket", one.clone()),
            (40197, "Unrecognized option to $bucket: a.".to_string())
        );
        assert_eq!(
            err("$bucketAuto", one.clone()),
            (40245, "Unrecognized option to $bucketAuto: a".to_string())
        );
        assert_eq!(
            err("$densify", one.clone()),
            (
                40415,
                "BSON field '$densify.a' is an unknown field.".to_string()
            )
        );
        assert_eq!(
            err("$fill", one.clone()),
            (
                40415,
                "BSON field '$fill.a' is an unknown field.".to_string()
            )
        );
        assert_eq!(
            err("$unionWith", one),
            (
                40415,
                "BSON field '$unionWith.a' is an unknown field.".to_string()
            )
        );
    }

    #[test]
    fn it_wins_even_when_a_required_field_is_also_missing() {
        // `$bucket` here is missing BOTH `groupBy` and `boundaries`.
        assert_eq!(err("$bucket", doc_of(&[("a", Bson::Int32(1))])).0, 40197);
    }

    #[test]
    fn a_missing_required_field_is_reported_after_that_pass() {
        let empty = Bson::Document(bson::Document::new());
        assert_eq!(
            err("$replaceRoot", empty.clone()),
            (
                40414,
                "BSON field '$replaceRoot.newRoot' is missing but a required field".to_string()
            )
        );
        assert_eq!(
            err("$densify", empty.clone()),
            (
                40414,
                "BSON field '$densify.field' is missing but a required field".to_string()
            )
        );
        assert_eq!(
            err("$fill", empty.clone()),
            (
                40414,
                "BSON field '$fill.output' is missing but a required field".to_string()
            )
        );
        assert_eq!(
            err("$out", empty.clone()),
            (
                40414,
                "BSON field '$out.coll' is missing but a required field".to_string()
            )
        );
        assert_eq!(
            err("$merge", empty.clone()),
            (
                40414,
                "BSON field '$merge.into' is missing but a required field".to_string()
            )
        );
        // `$sample` has its own wording rather than the IDL's.
        assert_eq!(
            err("$sample", empty),
            (28749, "$sample stage must specify a size".to_string())
        );
    }

    #[test]
    fn lookup_and_graph_lookup_report_a_missing_from_first() {
        let one = doc_of(&[("a", Bson::Int32(1))]);
        assert_eq!(
            err("$lookup", one.clone()),
            (
                9,
                "must specify 'pipeline' when 'from' is empty".to_string()
            )
        );
        // ... and `$graphLookup` ECHOES the spec, in mongod's SPACED document
        // rendering rather than the compact value one.
        assert_eq!(
            err("$graphLookup", one),
            (
                9,
                "missing 'from' option to $graphLookup stage specification: { a: 1 }".to_string()
            )
        );
        assert_eq!(
            err("$graphLookup", Bson::Document(bson::Document::new())),
            (
                9,
                "missing 'from' option to $graphLookup stage specification: {}".to_string()
            )
        );
    }

    #[test]
    fn but_an_unknown_field_wins_once_lookup_has_its_from() {
        let spec = doc_of(&[("from", Bson::String("c".into())), ("zz", Bson::Int32(1))]);
        assert_eq!(err("$lookup", spec).0, 40415);
    }

    #[test]
    fn geo_near_reports_a_missing_near_before_the_spec_type() {
        let expected = (5860400, "$geoNear requires a 'near' argument".to_string());
        assert_eq!(
            err("$geoNear", Bson::Document(bson::Document::new())),
            expected
        );
        assert_eq!(err("$geoNear", doc_of(&[("a", Bson::Int32(1))])), expected);
        // An ARRAY is a document in BSON, so it reaches the `near` check ...
        assert_eq!(err("$geoNear", Bson::Array(vec![])), expected);
        assert_eq!(err("$geoNear", Bson::Array(vec![Bson::Int32(1)])), expected);
        // ... while a SCALAR cannot be a spec at all.
        assert_eq!(err("$geoNear", Bson::Int32(5)).0, 10065);
    }

    #[test]
    fn group_reports_a_non_accumulator_before_the_missing_id() {
        assert_eq!(
            err("$group", doc_of(&[("a", Bson::Int32(1))])),
            (
                40234,
                "The field 'a' must be an accumulator object".to_string()
            )
        );
        assert_eq!(
            err("$group", Bson::Document(bson::Document::new())),
            (
                15955,
                "a group specification must include an _id".to_string()
            )
        );
    }

    #[test]
    fn union_with_asks_for_a_documents_pipeline_when_it_has_no_coll() {
        assert_eq!(
            err("$unionWith", Bson::Document(bson::Document::new())),
            (
                9,
                "$unionWith stage without explicit collection must have a pipeline \
                 with $documents as first stage"
                    .to_string()
            )
        );
    }

    #[test]
    fn unwind_names_an_unknown_option_before_the_missing_path() {
        assert_eq!(
            err("$unwind", doc_of(&[("a", Bson::Int32(1))])),
            (28811, "unrecognized option to $unwind stage: a".to_string())
        );
        // An EMPTY path is "no path", not a missing `$` prefix.
        assert_eq!(
            err("$unwind", Bson::String(String::new())),
            (28812, "no path specified to $unwind stage".to_string())
        );
    }

    #[test]
    fn empty_specs_that_are_errors_in_their_own_right() {
        assert_eq!(
            err("$project", Bson::Document(bson::Document::new())),
            (
                51272,
                "projection specification must have at least one field".to_string()
            )
        );
        assert_eq!(
            err("$sort", Bson::Document(bson::Document::new())),
            (
                15976,
                "$sort stage must have at least one sort key".to_string()
            )
        );
        assert_eq!(
            err("$count", Bson::String(String::new())),
            (
                40157,
                "the count field must be a non-empty string".to_string()
            )
        );
    }

    #[test]
    fn unset_rejects_the_shapes_that_used_to_unset_nothing() {
        assert_eq!(
            err("$unset", Bson::Array(vec![])),
            (
                31119,
                "$unset specification must be a string or an array with at least one field"
                    .to_string()
            )
        );
        assert_eq!(
            err("$unset", Bson::Array(vec![Bson::Int32(1)])),
            (
                31120,
                "$unset specification must be a string or an array containing only string values"
                    .to_string()
            )
        );
        assert_eq!(
            err("$unset", Bson::Int32(5)),
            (
                31002,
                "$unset specification must be a string or an array".to_string()
            )
        );
    }

    #[test]
    fn out_and_merge_name_their_own_argument_types() {
        assert_eq!(
            err("$out", Bson::Int32(5)),
            (
                16990,
                "$out only supports a string or object argument, but found int".to_string()
            )
        );
        // Different code AND different verb from `$out` -- probed, not assumed.
        assert_eq!(
            err("$merge", Bson::Int32(5)),
            (
                14,
                "$merge requires a string or object argument, but found int".to_string()
            )
        );
    }

    #[test]
    fn set_names_itself_not_the_alias_it_shares_an_implementation_with() {
        assert_eq!(
            err("$set", Bson::Int32(5)).1,
            "$set specification stage must be an object, got int"
        );
        assert_eq!(
            err("$addFields", Bson::Int32(5)).1,
            "$addFields specification stage must be an object, got int"
        );
    }

    #[test]
    fn valid_specs_are_left_alone() {
        // The guard against over-eager validation: each of these is legal, and
        // an arm that fired here would break working pipelines.
        ok("$sample", doc_of(&[("size", Bson::Int32(1))]));
        ok("$unset", Bson::String("a".into()));
        ok("$unset", Bson::Array(vec![Bson::String("a".into())]));
        ok("$unwind", Bson::String("$a".into()));
        ok("$unwind", doc_of(&[("path", Bson::String("$a".into()))]));
        ok("$group", doc_of(&[("_id", Bson::Null)]));
        ok("$project", doc_of(&[("a", Bson::Int32(1))]));
        ok("$sort", doc_of(&[("a", Bson::Int32(1))]));
        ok("$unionWith", Bson::String("c".into()));
        ok("$unionWith", doc_of(&[("coll", Bson::String("c".into()))]));
        ok("$out", Bson::String("c".into()));
        ok("$out", doc_of(&[("coll", Bson::String("c".into()))]));
        ok("$merge", doc_of(&[("into", Bson::String("c".into()))]));
        ok(
            "$bucket",
            doc_of(&[
                ("groupBy", Bson::String("$a".into())),
                (
                    "boundaries",
                    Bson::Array(vec![Bson::Int32(0), Bson::Int32(9)]),
                ),
            ]),
        );
        ok(
            "$bucketAuto",
            doc_of(&[
                ("groupBy", Bson::String("$a".into())),
                ("buckets", Bson::Int32(2)),
            ]),
        );
        ok(
            "$replaceRoot",
            doc_of(&[("newRoot", Bson::String("$$ROOT".into()))]),
        );
    }
}

#[cfg(test)]
mod stage_value_rendering_tests {
    //! mongod has THREE value renderings, not two, and both Rust renderers used
    //! to end in `other => format!("{other:?}")` -- so every type without an
    //! explicit arm reached the client as Rust `Debug`.

    use super::render_stage_value;
    use bson::{spec::BinarySubtype, Binary, Bson};

    #[test]
    fn the_types_that_used_to_render_as_rust_debug() {
        assert_eq!(
            render_stage_value(&Bson::RegularExpression(bson::Regex {
                pattern: "a".into(),
                options: String::new(),
            })),
            "/a/"
        );
        assert_eq!(
            render_stage_value(&Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![0x7A],
            })),
            // UNQUOTED here; the 40228 / 17053 family quotes it.
            "BinData(0, 7A)"
        );
        assert_eq!(
            render_stage_value(&Bson::Timestamp(bson::Timestamp {
                time: 1,
                increment: 1,
            })),
            "Timestamp(1, 1)"
        );
        assert_eq!(
            render_stage_value(&Bson::JavaScriptCode("x=1".into())),
            // Bare code text here; the other renderer wraps it as `Code("x=1")`.
            "x=1"
        );
        assert_eq!(render_stage_value(&Bson::MinKey), "MinKey");
        assert_eq!(render_stage_value(&Bson::MaxKey), "MaxKey");
    }

    #[test]
    fn nan_is_lower_case() {
        assert_eq!(render_stage_value(&Bson::Double(f64::NAN)), "nan");
    }

    #[test]
    fn the_arms_that_were_already_right_are_unchanged() {
        assert_eq!(render_stage_value(&Bson::String("x".into())), "\"x\"");
        assert_eq!(render_stage_value(&Bson::Int32(5)), "5");
        assert_eq!(render_stage_value(&Bson::Double(2.0)), "2.0");
        assert_eq!(
            render_stage_value(&Bson::Array(vec![Bson::Int32(1)])),
            "[ 1 ]"
        );
        assert_eq!(render_stage_value(&Bson::Array(vec![])), "[]");
    }
}

#[cfg(test)]
mod write_statement_tests {
    //! A malformed `q` / `u` used to fall through every match arm, so the
    //! statement APPLIED NOTHING and reported success. Probed against mongod
    //! 8.2.11 (2026-09-02) via `tools/probes/arg_types_documents.py`, which had
    //! no Rust column until then and so had never compared this server.

    use super::*;
    use bson::{doc, Bson};

    fn err(command: &str, spec: bson::Document) -> (i32, String) {
        let e = require_write_statement(&spec, command).expect_err("expected an error");
        (e.code, e.errmsg)
    }

    #[test]
    fn a_non_document_filter_is_a_type_mismatch() {
        for bad in [
            Bson::Int32(5),
            Bson::String("x".into()),
            Bson::Boolean(true),
        ] {
            let (code, msg) = err("update", doc! { "q": bad.clone(), "u": doc! {} });
            assert_eq!(code, 14);
            assert!(
                msg.starts_with("BSON field 'update.updates.q' is the wrong type"),
                "{msg}"
            );
            assert_eq!(err("delete", doc! { "q": bad, "limit": 0 }).0, 14);
        }
    }

    #[test]
    fn an_array_filter_is_a_type_mismatch_too() {
        let (code, msg) = err("update", doc! { "q": vec![1, 2], "u": doc! {} });
        assert_eq!(code, 14);
        assert!(msg.contains("the wrong type 'array'"), "{msg}");
    }

    #[test]
    fn a_non_document_update_is_failed_to_parse_not_a_type_mismatch() {
        for bad in [
            Bson::Int32(5),
            Bson::String("x".into()),
            Bson::Boolean(true),
        ] {
            assert_eq!(
                err("update", doc! { "q": doc! {}, "u": bad }),
                (
                    9,
                    "Update argument must be either an object or an array".to_string()
                )
            );
        }
    }

    #[test]
    fn an_array_update_is_a_pipeline_whose_elements_must_be_documents() {
        // Legal: a pipeline, including an empty one.
        require_write_statement(&doc! { "q": doc! {}, "u": Vec::<Bson>::new() }, "update").unwrap();
        require_write_statement(
            &doc! { "q": doc! {}, "u": vec![doc! {"$set": {"a": 1}}] },
            "update",
        )
        .unwrap();
        // A non-document element is 14, NOT the 9 a non-array `u` gets.
        assert_eq!(
            err("update", doc! { "q": doc! {}, "u": vec![Bson::Int32(1)] }),
            (
                14,
                "Each element of the 'pipeline' array must be an object".to_string()
            )
        );
    }

    #[test]
    fn an_absent_q_or_u_is_a_missing_required_field() {
        assert_eq!(
            err("update", doc! { "u": doc! {} }),
            (
                40414,
                "BSON field 'update.updates.q' is missing but a required field".to_string()
            )
        );
        assert_eq!(
            err("update", doc! { "q": doc! {} }),
            (
                40414,
                "BSON field 'update.updates.u' is missing but a required field".to_string()
            )
        );
        // `delete` has no `u`, so it stops after `q`.
        assert_eq!(
            err("delete", doc! { "limit": 0 }),
            (
                40414,
                "BSON field 'delete.deletes.q' is missing but a required field".to_string()
            )
        );
    }

    #[test]
    fn well_formed_statements_pass() {
        require_write_statement(
            &doc! { "q": doc! {"a": 1}, "u": doc! {"$set": {"b": 2}} },
            "update",
        )
        .unwrap();
        require_write_statement(&doc! { "q": doc! {}, "limit": 0 }, "delete").unwrap();
    }
}

#[cfg(test)]
mod expression_shape_tests {
    //! Expression arity and spec shape are PARSE errors, carrying the stage's
    //! wrapper rather than the optimizer's. Mirrors the Python server's
    //! `aggregate._expression_shape_problem`; both took this fix on 2026-09-02.
    //!
    //! Pinned against mongod 8.2.11 via `tools/probes/agg_expressions.py`,
    //! which went from 1,376 divergent shapes on this server to 981.

    use super::*;
    use bson::{doc, Bson};

    fn problem(expr: Bson) -> (i32, String) {
        expression_shape_problem(&expr).expect("expected a parse error")
    }

    #[test]
    fn arity_names_the_bounds_and_the_count() {
        assert_eq!(
            problem(bson::bson!({"$indexOfArray": [[1, 2]]})),
            (
                28667,
                "Expression $indexOfArray takes at least 2 arguments, and at most 4, \
                 but 1 were passed in."
                    .to_string()
            )
        );
        assert_eq!(
            problem(bson::bson!({"$range": [1]})).0,
            28667,
            "$range is 2..3, not 2..4 -- the bounds are per operator"
        );
    }

    #[test]
    fn a_non_array_argument_counts_as_one() {
        assert_eq!(problem(bson::bson!({"$indexOfCP": "x"})).0, 28667);
    }

    #[test]
    fn a_date_extractor_takes_a_one_element_array_or_a_bare_value() {
        // Legal: exactly one element, or not an array at all.
        assert_eq!(
            expression_shape_problem(&bson::bson!({"$year": ["$d"]})),
            None
        );
        assert_eq!(
            expression_shape_problem(&bson::bson!({"$year": "$d"})),
            None
        );
        for n in [0usize, 2, 3] {
            let arg: Vec<Bson> = (0..n).map(|_| Bson::Int32(1)).collect();
            assert_eq!(
                problem(bson::bson!({"$dayOfMonth": arg})),
                (
                    40536,
                    format!(
                        "$dayOfMonth accepts exactly one argument if given an array, \
                         but was given {n}"
                    )
                )
            );
        }
    }

    #[test]
    fn each_object_spec_expression_carries_its_own_code() {
        for (op, code) in [
            ("$firstN", 5787801),
            ("$lastN", 5787801),
            ("$minN", 5787900),
            ("$maxN", 5787900),
            ("$median", 7436201),
            ("$percentile", 7436200),
            ("$topN", 168),
            ("$bottomN", 168),
        ] {
            let mut d = bson::Document::new();
            d.insert(op, 0);
            let (got, msg) = problem(Bson::Document(d));
            assert_eq!(got, code, "{op}");
            assert_eq!(
                msg,
                format!("specification must be an object; found {op}: 0")
            );
        }
    }

    #[test]
    fn an_unrecognised_date_argument_is_named() {
        // Eight operators, eight codes -- they do not share one.
        for (op, code) in [
            ("$dateAdd", 5166401),
            ("$dateSubtract", 5166401),
            ("$dateDiff", 5166302),
            ("$dateFromParts", 40518),
            ("$dateToParts", 40520),
            ("$dateFromString", 40541),
            ("$dateToString", 18534),
            ("$dateTrunc", 5439008),
        ] {
            let mut d = bson::Document::new();
            d.insert(op, doc! {"k": 1});
            let (got, msg) = problem(Bson::Document(d));
            assert_eq!(got, code, "{op}");
            assert!(
                msg.starts_with(&format!("Unrecognized argument to {op}: k")),
                "{msg}"
            );
        }
    }

    #[test]
    fn three_of_them_append_what_they_expected() {
        let (_, msg) = problem(bson::bson!({"$dateAdd": {"k": 1}}));
        assert!(
            msg.ends_with(
                ". Expected arguments are startDate, unit, amount, and optionally timezone."
            ),
            "{msg}"
        );
        let (_, msg) = problem(bson::bson!({"$dateDiff": {"k": 1}}));
        assert!(msg.ends_with("$dateDiff: k"), "no tail on $dateDiff: {msg}");
    }

    #[test]
    fn it_recurses_into_nested_expressions() {
        assert_eq!(
            problem(bson::bson!({"$add": [1, {"$range": [1]}]})).0,
            28667
        );
    }

    #[test]
    fn well_formed_expressions_are_left_alone() {
        // The guard against over-eager validation: each of these is legal.
        for expr in [
            bson::bson!({"$indexOfCP": ["abc", "b"]}),
            bson::bson!({"$range": [0, 3]}),
            bson::bson!({"$slice": [[1, 2, 3], 2]}),
            bson::bson!({"$firstN": {"n": 1, "input": "$a"}}),
            bson::bson!({"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 1}}),
            bson::bson!({"$dateTrunc": {"date": "$d", "unit": "day", "binSize": 2}}),
            bson::bson!({"$year": "$d"}),
            bson::bson!({"$add": [1, 2]}),
        ] {
            assert_eq!(expression_shape_problem(&expr), None, "{expr:?}");
        }
    }
}

#[cfg(test)]
mod parse_time_required_keys {
    use super::*;
    use bson::{bson, Bson};

    fn probe(expr: Bson) -> Option<(i32, String, String)> {
        expression_problem_in_pipeline(&[bson!({"$addFields": {"z": expr}})], &[])
    }

    /// A missing required key is a PARSE error and takes the stage's wrapper.
    /// Every code and wording is measured mongod output (8.2.11, 2026-09-05).
    #[test]
    fn a_missing_required_key_is_a_parse_error() {
        for (expr, code, message) in [
            (
                bson!({"$convert": {"to": "int"}}),
                9,
                "Missing 'input' parameter to $convert",
            ),
            (
                bson!({"$dateDiff": {"unit": "day"}}),
                5166303,
                "Missing 'startDate' parameter to $dateDiff",
            ),
            (
                bson!({"$firstN": {"input": [1]}}),
                5787906,
                "Missing value for 'n'",
            ),
            (
                bson!({"$lastN": {"input": [1]}}),
                5787906,
                "Missing value for 'n'",
            ),
            (
                bson!({"$maxN": {"input": [1]}}),
                5787906,
                "Missing value for 'n'",
            ),
            (
                bson!({"$minN": {"input": [1]}}),
                5787906,
                "Missing value for 'n'",
            ),
            (
                bson!({"$dateFromParts": {"month": 1}}),
                40516,
                "$dateFromParts requires either 'year' or 'isoWeekYear' to be present",
            ),
        ] {
            let got = probe(expr.clone());
            assert_eq!(
                got,
                Some((code, message.to_string(), "$addFields".to_string())),
                "expr={expr:?}"
            );
        }
    }

    /// The ordering rule. mongod reports an UNRECOGNISED key before a MISSING
    /// required one, and both checks fire on the same document -- only their
    /// order separates them. Running the missing-key checks first changed the
    /// code on exactly these shapes when it was tried on the Python side.
    #[test]
    fn an_unknown_key_is_reported_before_a_missing_one() {
        for (expr, code) in [
            (bson!({"$dateDiff": {"k": 1}}), 5166302),
            (bson!({"$firstN": {"k": 1}}), 5787901),
            (bson!({"$lastN": {"k": 1}}), 5787901),
            (bson!({"$maxN": {"k": 1}}), 5787901),
            (bson!({"$minN": {"k": 1}}), 5787901),
            (bson!({"$dateFromParts": {"k": 1}}), 40518),
        ] {
            let got = probe(expr.clone());
            assert_eq!(
                got.as_ref().map(|(c, _, _)| *c),
                Some(code),
                "expr={expr:?}"
            );
        }
    }

    /// The guard against over-matching: a valid spec must fall through to the
    /// fold path, not be reported as a parse error.
    #[test]
    fn a_valid_spec_has_no_parse_time_problem() {
        for expr in [
            bson!({"$convert": {"input": "$a", "to": "int"}}),
            bson!({"$firstN": {"input": [1], "n": 1}}),
            bson!({"$dateFromParts": {"year": 2026}}),
            bson!({"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "day"}}),
        ] {
            assert_eq!(probe(expr.clone()), None, "expr={expr:?}");
        }
    }
}

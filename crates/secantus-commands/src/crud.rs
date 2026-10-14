//! The CRUD write/count command family: `insert`, `update`, `delete`, `count`.
//!
//! Faithful ports of `commands.py::_insert` / `_update` / `_delete` / `_count`,
//! scoped to the paths that map onto the current `secantus-storage` signatures.
//! (`find` lives in the sibling `find` module.)
//!
//! `insert` enforces a collection `validator` (code 121) unless
//! `bypassDocumentValidation` / `validationAction: warn|off`. Pipeline-form `u`
//! applies via `Storage::update_matching_pipeline`; positional operators (`$` /
//! `$[]` / `$[ident]`) + `arrayFilters` via `Storage::update_matching_array_filters`;
//! `let` + `collation` thread through update / delete / count (collation forces a
//! COLLSCAN; non-ASCII / numericOrdering collation → `BadValue`).
//!
//! `writeConcern` validation (malformed `w` / `j` / `wtimeout`) runs in `dispatch`
//! before the handler (`validate_write_concern`), and `writeConcernError`
//! attachment for a satisfiable-but-too-wide `w > 1` is added after
//! (`attach_write_concern_error`). `validator` enforcement runs on `insert` (here)
//! and on `update` / replace (the validator is read here and threaded into the
//! storage update to check the post-apply doc).
//!
//! Direct writes to the synthetic read-only views `local.oplog.rs` /
//! `admin.system.users` are rejected in `dispatch` (`reject_synthetic_view_write`,
//! code 13); reads on a user view (`find` / `aggregate` / `count`) resolve the
//! view's `viewOn` + pipeline in the aggregate/find command layer
//! (`aggregate::resolve_view`).

use bson::{doc, Bson, Document};

/// The `errInfo` body for a failed document validation: `failingDocumentId`
/// (when the doc has an `_id`) plus mongod's explanation of every rule it broke
/// (`validation_errors`). Shared with `findandmodify`.
pub(crate) fn validation_error_info(validator: &Document, doc: &Document) -> Document {
    let mut info = Document::new();
    if let Some(id) = doc.get("_id") {
        info.insert("failingDocumentId", id.clone());
    }
    info.insert("details", crate::validation_errors::explain(validator, doc));
    info
}

use crate::argtypes;
use crate::util::{
    as_i64, bool_field, coll_arg, command_error, doc_field, resolve_let_vars, write_error,
};
use crate::{CommandContext, CommandError, HandlerResult, StorageError};

/// Build the `insert` reply from storage's outcome: remap storage's
/// surviving-subset indices back to their original `documents` positions and
/// prepend the command-layer pre-check errors. Shared by the decoded and the
/// raw-BSON write paths.
fn insert_reply(
    inserted: usize,
    errors: Vec<Document>,
    pre_errors: Vec<Document>,
    survivor_to_orig: &[usize],
    ordered: bool,
) -> HandlerResult {
    let mut reply = doc! { "n": inserted as i32, "ok": 1.0 };
    // In an ordered batch a storage error (a duplicate key, say) stopped the
    // batch before the pre-check failure further on was ever reached.
    let pre_errors = if ordered && !errors.is_empty() {
        Vec::new()
    } else {
        pre_errors
    };
    if !pre_errors.is_empty() || !errors.is_empty() {
        let mut write_errors: Vec<Bson> = pre_errors.into_iter().map(Bson::Document).collect();
        for mut err in errors {
            if let Some(local) = err.get("index").and_then(as_i64) {
                let orig = survivor_to_orig
                    .get(local as usize)
                    .copied()
                    .unwrap_or(local as usize);
                err.insert("index", orig as i32);
            }
            write_errors.push(Bson::Document(err));
        }
        // By position in the batch, whichever check caught each.
        write_errors.sort_by_key(|e| {
            e.as_document()
                .and_then(|d| d.get("index"))
                .and_then(as_i64)
                .unwrap_or(0)
        });
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

/// An `_id` of a type mongod cannot hold: 53 `InvalidIdField`, measured
/// 8.2.11 (2026-10-10) for an array and a regex. Both were stored.
fn id_type_error(index: usize, type_name: &str) -> Document {
    doc! {
        "index": index as i32,
        "code": 53,
        "errmsg": format!("The '_id' value cannot be of type {type_name}"),
    }
}

/// The first `$`-prefixed field name inside an `_id` document, at any depth
/// and through arrays, in document order. A DBRef's `$ref` / `$id` / `$db`
/// are allowed where the document has both `$ref` and `$id`. Measured 8.2.11
/// (2026-10-12): `{_id: {a: {$b: 1}}}` is refused for `$b`, and
/// `{_id: {x: 1, $y: 2}}` for `$y`. Only the top level was looked at, and the
/// name reported was the document's first key.
fn id_dollar_field(id: &Document) -> Option<String> {
    fn within(value: &Bson) -> Option<String> {
        match value {
            Bson::Document(d) => id_dollar_field(d),
            Bson::Array(items) => items.iter().find_map(within),
            _ => None,
        }
    }
    let dbref = id.contains_key("$ref") && id.contains_key("$id");
    for (key, value) in id {
        let reference = dbref && matches!(key.as_str(), "$ref" | "$id" | "$db");
        if key.starts_with('$') && !reference {
            return Some(key.clone());
        }
        if let Some(found) = within(value) {
            return Some(found);
        }
    }
    None
}

fn id_dollar_error(index: usize, field: &str) -> Document {
    doc! {
        "index": index as i32,
        "code": 52,
        "errmsg": format!(
            "_id fields may not contain '$'-prefixed fields: {field} is not valid for storage."
        ),
    }
}

/// The deepest a stored document may nest, the document itself counting as
/// one. mongod refuses an insert past it with 15 (measured 8.2.11,
/// 2026-10-12); these were stored.
const MAX_NESTING_DEPTH: usize = 180;

/// A document nested to the limit holds at least this many bytes: eight for
/// each level. A shorter one is not walked.
const DEEP_DOCUMENT_BYTES: usize = MAX_NESTING_DEPTH * 8;

fn too_deep(value: &Bson, above: usize) -> bool {
    match value {
        Bson::Document(d) => {
            above + 1 > MAX_NESTING_DEPTH || d.values().any(|v| too_deep(v, above + 1))
        }
        Bson::Array(a) => above + 1 > MAX_NESTING_DEPTH || a.iter().any(|v| too_deep(v, above + 1)),
        _ => false,
    }
}

fn raw_too_deep(value: bson::RawBsonRef<'_>, above: usize) -> bool {
    match value {
        bson::RawBsonRef::Document(d) => {
            above + 1 > MAX_NESTING_DEPTH
                || d.iter().flatten().any(|(_, v)| raw_too_deep(v, above + 1))
        }
        bson::RawBsonRef::Array(a) => {
            above + 1 > MAX_NESTING_DEPTH
                || a.into_iter().flatten().any(|v| raw_too_deep(v, above + 1))
        }
        _ => false,
    }
}

/// What `insert` refuses about a document before it reaches storage: an
/// `_id` of a type mongod cannot hold, a `$`-prefixed field inside an `_id`
/// document, nesting past the limit. For the stages that insert (`$out`,
/// `$merge`), which do not go through the command.
pub(crate) fn insert_document_error(d: &Document) -> Option<(i32, String)> {
    let text = |e: Document| {
        (
            e.get_i32("code").unwrap_or(2),
            e.get_str("errmsg").unwrap_or_default().to_string(),
        )
    };
    match d.get("_id") {
        Some(Bson::Array(_)) => return Some(text(id_type_error(0, "array"))),
        Some(Bson::RegularExpression(_)) => return Some(text(id_type_error(0, "regex"))),
        Some(Bson::Document(id)) => {
            if let Some(field) = id_dollar_field(id) {
                return Some(text(id_dollar_error(0, &field)));
            }
        }
        _ => {}
    }
    d.values()
        .any(|v| too_deep(v, 1))
        .then(|| text(too_deep_error(0)))
}

fn too_deep_error(index: usize) -> Document {
    doc! {
        "index": index as i32,
        "code": 15,
        "errmsg": format!(
            "cannot insert document because it exceeds {MAX_NESTING_DEPTH} levels of nesting"
        ),
    }
}

/// The `_id` and nesting pre-checks run over raw BSON (no full decode): an
/// `_id` mongod cannot hold, a `$`-prefixed field inside an `_id` document,
/// a document nested past the limit.
fn raw_document_error(raw: &[u8], index: usize) -> Option<Document> {
    let rd = bson::RawDocument::from_bytes(raw).ok()?;
    match rd.get("_id") {
        Ok(Some(bson::RawBsonRef::Document(idd))) => {
            if let Some(field) = Document::try_from(idd)
                .ok()
                .and_then(|id| id_dollar_field(&id))
            {
                return Some(id_dollar_error(index, &field));
            }
        }
        Ok(Some(bson::RawBsonRef::Array(_))) => return Some(id_type_error(index, "array")),
        Ok(Some(bson::RawBsonRef::RegularExpression(_))) => {
            return Some(id_type_error(index, "regex"))
        }
        _ => {}
    }
    if raw.len() >= DEEP_DOCUMENT_BYTES && raw_too_deep(bson::RawBsonRef::Document(rd), 0) {
        return Some(too_deep_error(index));
    }
    None
}

/// A double as C's `%g` writes it: six significant digits, trailing zeros
/// dropped, an exponent below 1e-4 and from 1e+6. mongod prints a `limit` it
/// refuses this way: `2.14748e+09`, `1e+06`, `0.1`.
pub(crate) fn fmt_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return (if x > 0.0 { "inf" } else { "-inf" }).to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    let rounded = format!("{x:.5e}");
    let (mantissa, exp) = rounded.split_once('e').unwrap_or((&rounded, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        trim(&format!("{x:.*}", (5 - exp) as usize))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    }
}

/// A delete statement's `limit`: 0 removes every match and 1 the first.
/// Any other number is refused, written as `%g` writes it; a value that is
/// not a number reads as before.
fn delete_limit(value: Option<&Bson>) -> Result<usize, CommandError> {
    let number = match value {
        Some(Bson::Int32(n)) => f64::from(*n),
        Some(Bson::Int64(n)) => *n as f64,
        Some(Bson::Double(n)) => *n,
        Some(Bson::Decimal128(n)) => n.to_string().parse().unwrap_or(f64::NAN),
        other => other.and_then(as_i64).unwrap_or(0) as f64,
    };
    if number == 0.0 {
        Ok(0)
    } else if number == 1.0 {
        Ok(1)
    } else {
        Err(CommandError::new(
            9,
            "FailedToParse",
            format!(
                "The limit field in delete objects must be 0 or 1. Got {}",
                fmt_g(number)
            ),
        ))
    }
}

/// What mongod refuses about an `insert` or `delete` command's own fields
/// before it runs anything, reading the fields in the order the command has
/// them: an unknown field, a wrong type, a statement that is not a document
/// or lacks `q` or `limit`, a `limit` that is not 0 or 1. Measured 8.2.11,
/// 2026-10-12. Each was accepted before, and for `delete` that meant a
/// statement with no `limit` removed every matching document, and the
/// statements ahead of a bad one were applied.
///
/// `in_sequence` says the statements arrived in a document sequence of the
/// message and are not in the command body.
fn write_command_problem(doc: &Document, command: &str, in_sequence: bool) -> Option<CommandError> {
    const INSERT_FIELDS: &[&str] = &[
        "documents",
        "ordered",
        "bypassDocumentValidation",
        "stmtIds",
        "encryptionInformation",
        "bypassEmptyTsReplacement",
        "collectionUUID",
        "originalQuery",
        "originalCollation",
        "rawData",
    ];
    const DELETE_FIELDS: &[&str] = &[
        "deletes",
        "ordered",
        "bypassDocumentValidation",
        "let",
        "stmtIds",
        "encryptionInformation",
        "bypassEmptyTsReplacement",
        "runtimeConstants",
        "legacyRuntimeConstants",
        "collectionUUID",
        "originalQuery",
        "originalCollation",
        "rawData",
    ];
    const DELETE_STATEMENT_FIELDS: &[&str] = &["q", "limit", "collation", "hint", "sampleId"];
    let (array, known) = if command == "insert" {
        ("documents", INSERT_FIELDS)
    } else {
        ("deletes", DELETE_FIELDS)
    };
    let wrong_type = |path: String, value: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
                secantus_core::query::bson_type_name(value)
            ),
        )
    };
    let missing = |path: String| {
        CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{path}' is missing but a required field"),
        )
    };
    let unknown = |path: String| {
        CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field '{path}' is an unknown field."),
        )
    };
    let statement_problem = |statement: &Document| -> Option<CommandError> {
        for (key, value) in statement {
            match key.as_str() {
                "q" if !matches!(value, Bson::Null | Bson::Document(_)) => {
                    return Some(wrong_type("delete.deletes.q".to_string(), value, "object"))
                }
                "collation" if !matches!(value, Bson::Null | Bson::Document(_)) => {
                    return Some(wrong_type(
                        "delete.deletes.collation".to_string(),
                        value,
                        "object",
                    ))
                }
                "hint" if !matches!(value, Bson::String(_) | Bson::Document(_)) => {
                    return Some(CommandError::new(
                        9,
                        "FailedToParse",
                        "Hint must be a string or an object",
                    ))
                }
                "sampleId" if !matches!(value, Bson::Null | Bson::Binary(_)) => {
                    return Some(wrong_type(
                        "delete.deletes.sampleId".to_string(),
                        value,
                        "binData",
                    ))
                }
                name if !name.starts_with('$') && !DELETE_STATEMENT_FIELDS.contains(&name) => {
                    return Some(unknown(format!("delete.deletes.{name}")))
                }
                _ => {}
            }
        }
        if !statement.contains_key("limit") {
            return Some(missing("delete.deletes.limit".to_string()));
        }
        if matches!(statement.get("q"), None | Some(Bson::Null)) {
            return Some(missing("delete.deletes.q".to_string()));
        }
        delete_limit(statement.get("limit")).err()
    };
    let mut given = in_sequence;
    for (key, value) in doc {
        match key.as_str() {
            name if name == command || name.starts_with('$') => {}
            name if name == array => match value {
                Bson::Null => {}
                Bson::Array(items) => {
                    given = true;
                    for (index, item) in items.iter().enumerate() {
                        match item {
                            // A null among them is no statement at all.
                            Bson::Null => {}
                            Bson::Document(statement) => {
                                if command == "delete" {
                                    if let Some(problem) = statement_problem(statement) {
                                        return Some(problem);
                                    }
                                }
                            }
                            other => {
                                return Some(wrong_type(
                                    format!("{command}.{array}.{index}"),
                                    other,
                                    "object",
                                ))
                            }
                        }
                    }
                }
                other => return Some(wrong_type(format!("{command}.{array}"), other, "array")),
            },
            "ordered" => {
                if let Err(e) =
                    argtypes::require_bool_field(doc, "ordered", &format!("{command}.ordered"))
                {
                    return Some(e);
                }
            }
            "bypassDocumentValidation" => {
                if let Err(e) = argtypes::require_bool_or_number(
                    doc,
                    "bypassDocumentValidation",
                    &format!("{command}.bypassDocumentValidation"),
                ) {
                    return Some(e);
                }
            }
            "bypassEmptyTsReplacement" => {
                if let Some(e) = empty_ts_flag_problem(doc) {
                    return Some(e);
                }
            }
            "let" if command == "delete" => {
                if !matches!(value, Bson::Null | Bson::Document(_)) {
                    return Some(wrong_type("delete.let".to_string(), value, "object"));
                }
            }
            "isTimeseriesNamespace" => {
                if matches!(value, Bson::Boolean(true)) {
                    return Some(CommandError::new(
                        5916400,
                        "Location5916400",
                        "'isTimeseriesNamespace' parameter can only be set when the request is \
                         sent on system.buckets namespace",
                    ));
                }
            }
            "encryptionInformation" => {
                if matches!(value, Bson::Document(e) if !e.contains_key("schema")) {
                    return Some(missing(format!("{command}.encryptionInformation.schema")));
                }
            }
            name => {
                if !known.contains(&name) && !argtypes::is_generic_field(name) {
                    return Some(unknown(format!("{command}.{name}")));
                }
            }
        }
    }
    if !given {
        return Some(missing(format!("{command}.{array}")));
    }
    None
}

/// `bypassEmptyTsReplacement` is a bool on every write command that takes it.
fn empty_ts_flag_problem(doc: &Document) -> Option<CommandError> {
    match doc.get("bypassEmptyTsReplacement") {
        None | Some(Bson::Null) | Some(Bson::Boolean(_)) => None,
        Some(other) => Some(CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "Field 'bypassEmptyTsReplacement' should be a boolean value, but found: {}",
                secantus_core::query::bson_type_name(other)
            ),
        )),
    }
}

/// The documents or statements of a write, without the nulls mongod drops.
fn write_statements<'a>(doc: &'a Document, field: &str) -> Vec<&'a Bson> {
    match doc.get(field) {
        Some(Bson::Array(items)) => items.iter().filter(|v| !matches!(v, Bson::Null)).collect(),
        _ => Vec::new(),
    }
}

fn empty_batch_error() -> CommandError {
    CommandError::new(
        16,
        "InvalidLength",
        "Write batch sizes must be between 1 and 100000. Got 0 operations.",
    )
}

/// Raw-BSON write path: store the client's insert documents verbatim (the byte
/// slices the server handed in un-decoded) without the merge-decode → re-encode
/// → storage-decode round-trip. Only the `_id` `$`-key pre-check runs over every
/// document; a collection `validator` is the one case that forces a per-doc
/// decode-and-match before the bytes go to storage.
/// `doc` with every top-level `Timestamp(0, 0)` replaced by the current one,
/// or `None` when it holds none.
///
/// mongod fills an empty timestamp in a document it is given whole -- an
/// insert, a replacement, an upsert of a replacement -- and only at the top
/// level, and never the `_id`. A `$set` of `Timestamp(0, 0)` stores it as
/// written (measured 8.2.11, 2026-10-12). This server stored it in every case.
pub(crate) fn fill_empty_timestamps(
    doc: &Document,
    storage: &dyn crate::Storage,
) -> Option<Document> {
    let empty = |v: &Bson| matches!(v, Bson::Timestamp(t) if t.time == 0 && t.increment == 0);
    if !doc.iter().any(|(k, v)| k != "_id" && empty(v)) {
        return None;
    }
    let now = Bson::Timestamp(storage.current_cluster_time());
    let mut filled = Document::new();
    for (k, v) in doc {
        let value = if k != "_id" && empty(v) {
            now.clone()
        } else {
            v.clone()
        };
        filled.insert(k.clone(), value);
    }
    Some(filled)
}

/// Does this encoded document hold a top-level `Timestamp(0, 0)` to fill?
fn raw_has_empty_timestamp(raw: &[u8]) -> bool {
    let Ok(doc) = bson::RawDocument::from_bytes(raw) else {
        return false;
    };
    doc.iter().flatten().any(|(k, v)| {
        k != "_id" && matches!(v, bson::RawBsonRef::Timestamp(t) if t.time == 0 && t.increment == 0)
    })
}

fn insert_raw_path(
    db: &str,
    coll: &str,
    raw_docs: Vec<Vec<u8>>,
    ordered: bool,
    validator: Option<&Document>,
    fill_timestamps: bool,
    storage: &dyn crate::Storage,
) -> HandlerResult {
    if raw_docs.is_empty() {
        return Ok(doc! {
            "ok": 0.0,
            "errmsg": "Write batch sizes must be between 1 and 100000. Got 0 operations.",
            "code": 16,
            "codeName": "InvalidLength",
        });
    }
    let mut pre_errors: Vec<Document> = Vec::new();
    let mut surviving: Vec<Vec<u8>> = Vec::new();
    let mut survivor_to_orig: Vec<usize> = Vec::new();
    for (index, raw) in raw_docs.into_iter().enumerate() {
        if let Some(err) = raw_document_error(&raw, index) {
            pre_errors.push(err);
            if ordered {
                break;
            }
            continue;
        }
        if let Some(v) = validator {
            // The one case the raw path must decode: match against the validator.
            // Wire-validated BSON always decodes; if it somehow doesn't, fall through
            // and let storage surface the error rather than dropping the doc.
            if let Ok(d) = secantus_core::bsondoc::from_bytes(&raw[..]) {
                if !secantus_core::query::matches(&d, v, &Document::new(), None).unwrap_or(true) {
                    pre_errors.push(doc! {
                        "index": index as i32,
                        "code": 121,
                        "errmsg": "Document failed validation",
                        "errInfo": validation_error_info(v, &d),
                    });
                    if ordered {
                        break;
                    }
                    continue;
                }
            }
        }
        let raw = if fill_timestamps && raw_has_empty_timestamp(&raw) {
            match secantus_core::bsondoc::from_bytes(&raw[..])
                .ok()
                .and_then(|d| fill_empty_timestamps(&d, storage))
            {
                Some(filled) => encode_doc(&filled)?,
                None => raw,
            }
        } else {
            raw
        };
        surviving.push(raw);
        survivor_to_orig.push(index);
    }

    // An ordered batch stops AT the document that failed a pre-check: the
    // documents before it are inserted (mongod 8.2.11 answers `n: 1` for a
    // three-document batch whose second fails its validator). They used to
    // be dropped with it.
    if surviving.is_empty() {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }
    let (inserted, errors) = storage
        .insert(db, coll, surviving, ordered)
        .map_err(command_error)?;
    insert_reply(inserted, errors, pre_errors, &survivor_to_orig, ordered)
}

/// `insert` — batch document insert.
/// mongod's empty-batch rule, which `insert` already applied and `update` /
/// `delete` did not: an absent, non-array or empty write array is
/// InvalidLength (16), not a no-op success. Both used to answer ok:1 with
/// `n: 0`, telling the caller a batch it never sent had been applied.
fn empty_batch(doc: &Document, field: &str) -> Result<(), CommandError> {
    match doc.get(field) {
        Some(Bson::Array(a)) if !a.is_empty() => Ok(()),
        _ => Err(CommandError::new(
            16,
            "InvalidLength",
            "Write batch sizes must be between 1 and 100000. Got 0 operations.",
        )),
    }
}

/// A `$text` query needs a text index, and this server builds none: mongod's
/// answer where there is none is 27 (measured 8.2.11, 2026-10-12).
pub(crate) fn text_query_problem(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    filter: &Document,
) -> Option<(i32, String)> {
    fn asks(filter: &Document) -> bool {
        filter.iter().any(|(key, value)| match key.as_str() {
            "$text" => true,
            "$and" | "$or" | "$nor" => matches!(value, Bson::Array(subs) if subs
                .iter()
                .any(|sub| matches!(sub, Bson::Document(d) if asks(d)))),
            _ => false,
        })
    }
    if !asks(filter) {
        return None;
    }
    if !storage.collection_exists(db, coll).unwrap_or(false) {
        return Some((
            27,
            format!("text index required for $text query (no such collection '{db}.{coll}')"),
        ));
    }
    // An index declared as text is not one this server can search; the
    // query is left to fail as it did.
    let declared = storage
        .list_indexes(db, coll)
        .unwrap_or_default()
        .iter()
        .any(|index| {
            index
                .get_document("key")
                .is_ok_and(|key| key.values().any(|v| v.as_str() == Some("text")))
        });
    (!declared).then(|| (27, "text index required for $text query".to_string()))
}

/// The fields `count` and `distinct` share with every sampled or mirrored
/// read: a bool `mirrored` and `rawData`, a binData `sampleId`, and an
/// `encryptionInformation` that has a `schema`. `path` is the name mongod
/// reports the command's fields under. Measured 8.2.11, 2026-10-12.
pub(crate) fn read_option_problem(doc: &Document, path: &str) -> Option<CommandError> {
    for (key, value) in doc {
        match key.as_str() {
            "mirrored" | "rawData" if !matches!(value, Bson::Null | Bson::Boolean(_)) => {
                return Some(CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "Field '{key}' should be a boolean value, but found: {}",
                        secantus_core::query::bson_type_name(value)
                    ),
                ))
            }
            "sampleId" if !matches!(value, Bson::Null | Bson::Binary(_)) => {
                return Some(CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "BSON field '{path}.sampleId' is the wrong type '{}', expected type \
                         'binData'",
                        secantus_core::query::bson_type_name(value)
                    ),
                ))
            }
            "encryptionInformation" => {
                if matches!(value, Bson::Document(e) if !e.contains_key("schema")) {
                    return Some(CommandError::new(
                        40414,
                        "IDLFailedToParse",
                        format!(
                            "BSON field '{path}.encryptionInformation.schema' is missing but a \
                             required field"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// `count`'s `limit` as mongod reads it: a whole number that fits 64 bits,
/// and not the smallest one (its size, which a negative limit means, does
/// not fit). Measured 8.2.11, 2026-10-12.
fn count_limit(value: Option<&Bson>) -> Result<i64, CommandError> {
    let unfit = |shown: String| {
        CommandError::new(
            9,
            "FailedToParse",
            format!("Cannot represent as a 64-bit integer: limit: {shown}"),
        )
    };
    let whole = |x: f64| x.fract() == 0.0 && (-(2f64.powi(63))..2f64.powi(63)).contains(&x);
    let limit = match value {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Double(x)) => {
            if x.is_nan() {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    "Expected an integer, but found NaN in: limit: nan",
                ));
            }
            if x.is_finite() && x.fract() != 0.0 {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    format!("Expected an integer: limit: {x}"),
                ));
            }
            if !whole(*x) {
                return Err(unfit(secantus_core::query::fmt_double_parse(*x)));
            }
            *x as i64
        }
        Some(Bson::Decimal128(d)) => {
            let x: f64 = d.to_string().parse().unwrap_or(f64::NAN);
            if !whole(x) {
                return Err(unfit(d.to_string()));
            }
            x as i64
        }
        _ => 0,
    };
    if limit == i64::MIN {
        return Err(CommandError::new(
            2,
            "BadValue",
            "limit value for count cannot be min long",
        ));
    }
    Ok(limit)
}

/// `count`'s `skip` as mongod reads it: a double is cut to its whole part, a
/// decimal is rounded to the nearest whole number (a half to the even one),
/// NaN is 0, and a number past 64 bits is the largest that fits.
fn count_skip(value: Option<&Bson>) -> i64 {
    match value {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Double(x)) => *x as i64,
        Some(Bson::Decimal128(d)) => d
            .to_string()
            .parse::<f64>()
            .map_or(0, |x| x.round_ties_even() as i64),
        _ => 0,
    }
}

pub fn insert(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "insert")?;
    if let Some(e) = crate::admin::invalid_write_namespace(&ctx.db_name, &coll) {
        return Ok(e.into_reply());
    }
    argtypes::require_object(doc, "writeConcern", "insert.writeConcern")?;
    // Take the raw-BSON write side-channel first, releasing the `&mut` borrow
    // before the immutable `storage()` / `db_name` borrows below.
    let raw_docs = ctx.raw_insert_documents.take();
    if let Some(problem) = write_command_problem(doc, "insert", raw_docs.is_some()) {
        return Err(problem);
    }
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    let ordered = bool_field(doc, "ordered", true);
    // `bypassEmptyTsReplacement: true` stores a `Timestamp(0, 0)` as given.
    let fill_timestamps = !matches!(
        doc.get("bypassEmptyTsReplacement"),
        Some(Bson::Boolean(true))
    );
    // The buckets of a time-series collection that does not exist: mongod
    // will not create them from an insert (20, per document).
    if coll.starts_with("system.buckets.") && !storage.collection_exists(&db, &coll).unwrap_or(true)
    {
        let count = match &raw_docs {
            Some(raw) => raw.len(),
            None => write_statements(doc, "documents").len(),
        };
        if count == 0 {
            return Err(empty_batch_error());
        }
        let refused: Vec<Bson> = (0..if ordered { 1 } else { count })
            .map(|index| {
                Bson::Document(doc! {
                    "index": index as i32,
                    "code": 20,
                    "errmsg": "Creation of a timeseries bucket collection without timeseries \
                               options is not allowed",
                })
            })
            .collect();
        return Ok(doc! { "n": 0_i32, "writeErrors": refused, "ok": 1.0 });
    }
    // A validator compares under the collection's default collation.
    let default_collation = crate::util::default_collation(storage, &ctx.db_name, &coll);
    let _active = secantus_core::collation::activate(default_collation.as_ref());

    // Document validation: a collection `validator` rejects non-matching inserts
    // with code 121 unless `validationAction` is "warn"/"off" or
    // `bypassDocumentValidation` is set. A validator the query engine can't
    // evaluate is treated as passing (lenient) rather than wrongly rejecting.
    let bypass = bool_field(doc, "bypassDocumentValidation", false);
    let validator = if bypass {
        None
    } else {
        let opts = storage
            .get_collection_options(&db, &coll)
            .map_err(command_error)?;
        let action = opts.get_str("validationAction").unwrap_or("error");
        // `validationLevel: "off"` disables validation entirely, whatever the
        // action says — mongod checks the level first. The level was stored by
        // `create` / `collMod` and then never consulted, so a collection
        // explicitly opted OUT of validation still had it enforced.
        let level_off = opts.get_str("validationLevel").unwrap_or("strict") == "off";
        if level_off || action == "warn" || action == "off" {
            None
        } else {
            opts.get("validator").and_then(Bson::as_document).cloned()
        }
    };

    // A time-series collection checks every measurement's time field, so its
    // documents take the decoded path however they arrived.
    let ts_fields = storage
        .get_collection_options(&db, &coll)
        .ok()
        .and_then(|o| crate::timeseries::Fields::of(&o));
    // Raw-BSON write fast path: documents arrived as an un-decoded kind-1 sequence.
    let from_sequence: Option<Vec<Bson>> = match raw_docs {
        Some(raw_docs) if ts_fields.is_none() => {
            return insert_raw_path(
                &db,
                &coll,
                raw_docs,
                ordered,
                validator.as_ref(),
                fill_timestamps,
                storage,
            );
        }
        Some(raw_docs) => Some(
            raw_docs
                .iter()
                .map(|b| secantus_core::bsondoc::from_bytes(&b[..]).map(Bson::Document))
                .collect::<Result<_, _>>()
                .map_err(|e| CommandError::new(22, "InvalidBSON", e.to_string()))?,
        ),
        None => None,
    };

    // Decoded path: documents inline in the command body, without the nulls
    // mongod drops (`[{_id: 1}, null]` inserts one document).
    let documents: Vec<&Bson> = match &from_sequence {
        Some(a) => a.iter().collect(),
        None => write_statements(doc, "documents"),
    };
    // mongod rejects an empty `documents` array with InvalidLength, which is
    // code **16** — this said 4 (NoSuchKey) under a comment asserting drivers
    // gate on "this exact code/codeName combo". They gate on the codeName; the
    // code was simply wrong, and `bulkWrite` in this same crate already
    // answered 16. Probed on mongod 8.2.11.
    if documents.is_empty() {
        return Ok(empty_batch_error().into_reply());
    }

    // Per-doc pre-checks (no storage): an `_id` may not carry top-level
    // `$`-prefixed keys. A failure is a per-doc writeError keyed by the doc's
    // position in the original `documents` array.
    let mut pre_errors: Vec<Document> = Vec::new();
    let mut surviving: Vec<Vec<u8>> = Vec::new();
    let mut survivor_to_orig: Vec<usize> = Vec::new();
    for (index, entry) in documents.iter().copied().enumerate() {
        let Bson::Document(d) = entry else {
            // Non-document batch entries are malformed input; mongod rejects
            // them. Surface a per-doc BadValue rather than choking the batch.
            pre_errors.push(doc! {
                "index": index as i32,
                "code": 2,
                "errmsg": "insert document must be an object",
            });
            if ordered {
                break;
            }
            continue;
        };
        let id_type = match d.get("_id") {
            Some(Bson::Array(_)) => Some("array"),
            Some(Bson::RegularExpression(_)) => Some("regex"),
            _ => None,
        };
        // A time-series collection keys nothing by `_id`, and takes any.
        if let Some(type_name) = id_type.filter(|_| ts_fields.is_none()) {
            pre_errors.push(id_type_error(index, type_name));
            if ordered {
                break;
            }
            continue;
        }
        let id_field = match d.get("_id") {
            Some(Bson::Document(id_value)) => id_dollar_field(id_value),
            _ => None,
        };
        if let Some(field) = id_field {
            pre_errors.push(id_dollar_error(index, &field));
            if ordered {
                break;
            }
            continue;
        }
        if d.values().any(|v| too_deep(v, 1)) {
            pre_errors.push(too_deep_error(index));
            if ordered {
                break;
            }
            continue;
        }
        if let Some(problem) = ts_fields
            .as_ref()
            .and_then(|f| crate::timeseries::measurement_problem(d, f))
        {
            pre_errors.push(doc! { "index": index as i32, "code": 2, "errmsg": problem });
            if ordered {
                break;
            }
            continue;
        }
        // Collection validator (code 121, DocumentValidationFailure). mongod
        // attaches an `errInfo` (failingDocumentId + per-operator details) that
        // drivers' errorResponse tests read; mirror it.
        if let Some(v) = &validator {
            if !secantus_core::query::matches(d, v, &Document::new(), None).unwrap_or(true) {
                pre_errors.push(doc! {
                    "index": index as i32,
                    "code": 121,
                    "errmsg": "Document failed validation",
                    "errInfo": validation_error_info(v, d),
                });
                if ordered {
                    break;
                }
                continue;
            }
        }
        let filled = if fill_timestamps {
            fill_empty_timestamps(d, storage)
        } else {
            None
        };
        surviving.push(match filled {
            Some(filled) => encode_doc(&filled)?,
            None => encode_doc(d)?,
        });
        survivor_to_orig.push(index);
    }

    // An ordered batch stops AT the pre-check failure; the documents before
    // it are inserted (see `insert_raw_path`).
    if surviving.is_empty() {
        return Ok(doc! { "n": 0_i32, "ok": 1.0, "writeErrors": bson_array(pre_errors) });
    }

    let (inserted, errors) = storage
        .insert(&db, &coll, surviving, ordered)
        .map_err(command_error)?;
    insert_reply(inserted, errors, pre_errors, &survivor_to_orig, ordered)
}

/// `delete` — batch delete, one entry per `{q, limit}` spec.
pub fn delete(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "delete")?;
    if let Some(e) = crate::admin::invalid_write_namespace(&ctx.db_name, &coll) {
        return Ok(e.into_reply());
    }
    if let Some(problem) = write_command_problem(doc, "delete", false) {
        return Err(problem);
    }
    let deletes = write_statements(doc, "deletes");
    if deletes.is_empty() {
        return Err(empty_batch_error());
    }
    let storage = ctx.storage()?;
    // NOTE: an unresolvable `hint` is a PER-STATEMENT writeError, not a
    // command-level failure. mongod answers `ok: 1` with
    // `writeErrors: [{index, code: 2}]` and applies the other statements in the
    // batch; returning `Err` here failed the whole command and reported no
    // `n`. Probed 8.2.11 (2026-08-31) against a bogus index name on both
    // `delete` and `update`. Validation therefore lives in the loop below,
    // beside the other per-statement checks, and not in a pre-pass.
    let ordered = bool_field(doc, "ordered", true);

    let mut n = 0_i32;
    let mut write_errors: Vec<Bson> = Vec::new();
    // Command-level `let` → vars visible to `$expr` in each spec's `q` filter.
    let let_vars = resolve_let_vars(doc.get("let"));
    // A `let` mongod refuses is reported for each statement it reaches.
    let let_problem = match doc.get("let") {
        Some(Bson::Document(vars)) => argtypes::let_problem(vars),
        _ => None,
    };
    for (index, spec) in deletes.iter().copied().enumerate() {
        let Bson::Document(spec) = spec else { continue };
        let mut refused: Option<(i32, String)> = let_problem.clone();
        if refused.is_none() {
            // `{$natural: 2}`, or `$natural` beside another field.
            refused = argtypes::require_natural_hint(spec.get("hint"))
                .err()
                .map(|e| (e.code, e.errmsg));
        }
        if refused.is_none() {
            if let Some(Bson::Document(q)) = spec.get("q") {
                refused = text_query_problem(storage, &ctx.db_name, &coll, q);
            }
        }
        if let Some((code, errmsg)) = refused {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": code,
                "errmsg": errmsg,
            }));
            if ordered {
                break;
            }
            continue;
        }
        // As in `update`: an undefined `$$variable` in the filter is a
        // per-statement writeError carrying mongod's 17276.
        {
            let bound: Vec<String> = match doc.get("let") {
                Some(Bson::Document(d)) => d.keys().cloned().collect(),
                _ => Vec::new(),
            };
            if let Some(Bson::Document(q)) = spec.get("q") {
                if let Some((code, msg)) = argtypes::expression_problem_in_filter(q, &bound) {
                    write_errors.push(Bson::Document(doc! {
                        "index": index as i32,
                        "code": code,
                        "errmsg": msg,
                    }));
                    if ordered {
                        break;
                    }
                    continue;
                }
            }
        }
        // An unresolvable `hint` fails THIS statement, not the batch (see the
        // note above the loop).
        if let Err(e) =
            crate::util::validate_write_hint(storage, &ctx.db_name, &coll, spec.get("hint"))
        {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": e.code,
                "errmsg": e.errmsg.clone(),
            }));
            if ordered {
                break;
            }
            continue;
        }
        let filter = doc_field(spec, "q");
        // `collation` is per-delete-statement (inside each `deletes[]` entry).
        let collation = match crate::util::effective_collation(storage, &ctx.db_name, &coll, spec) {
            Ok(c) => c,
            Err(e) => {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32,
                    "code": e.code,
                    "errmsg": e.errmsg.clone(),
                }));
                if ordered {
                    break;
                }
                continue;
            }
        };
        let _active = secantus_core::collation::activate(collation.as_ref());
        // `limit: 0` ⇒ delete all matches, `1` ⇒ at most one. mongod defines
        // nothing else and refuses the whole command before any statement
        // runs (`write_command_problem`).
        let limit = delete_limit(spec.get("limit"))?;
        // A delete of ONE document removes the first its query matches in
        // the order mongod reads them.
        let first = if limit == 1 {
            crate::explain::order::write_candidates(
                storage,
                &crate::explain::order::Ask {
                    db: &ctx.db_name,
                    coll: &coll,
                    filter: &filter,
                    sort: None,
                    hint: argtypes::given_hint(spec),
                    collation: collation.as_ref(),
                    vars: &let_vars,
                    limit: 0,
                    asked: Default::default(),
                },
            )
            .unwrap_or(None)
        } else {
            None
        };
        let deleted = crate::explain::order::pinned(storage, first, || {
            storage.delete_matching_hinted(
                &ctx.db_name,
                &coll,
                &filter,
                limit,
                &let_vars,
                collation.as_ref(),
                argtypes::given_hint(spec),
            )
        });
        match deleted {
            Ok(deleted) => {
                n += deleted as i32;
                if deleted > 0 {
                    crate::transactions::note_statement(
                        index,
                        crate::transactions::StmtRecord::Delete,
                    );
                }
            }
            Err(StorageError::Internal(msg)) => {
                return Ok(CommandError::new(1, "InternalError", msg).into_reply())
            }
            // A write conflict fails the whole statement command-level (mongod
            // semantics), so the dispatch txn envelope attaches the transient
            // label and drivers retry the transaction.
            Err(e @ StorageError::WriteConflict) => return Ok(command_error(e).into_reply()),
            Err(e) => {
                write_errors.push(Bson::Document(write_error(index, e, "delete")));
                if ordered {
                    break;
                }
            }
        }
    }

    let mut reply = doc! { "n": n, "ok": 1.0 };
    if !write_errors.is_empty() {
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

/// `count` — count matching documents, honouring `skip` / `limit` clamping the
/// way mongod's `count` command (and the legacy `cursor.count()`) does.
pub fn count(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "count")?;
    argtypes::require_object(doc, "query", "count.query")?;
    argtypes::require_object(doc, "collation", "count.collation")?;
    // Two adjacent numeric slots, two families: `limit` answers BadValue with
    // its own wording and rejects null, `skip` answers the ordinary numeric
    // type error and accepts one. Probed on 8.2.11.
    argtypes::require_count_limit(doc, "limit")?;
    argtypes::require_number(doc, "skip", "count.skip")?;
    argtypes::require_hint(doc, "hint")?;
    const FIELDS: &[&str] = &[
        "count",
        "query",
        "limit",
        "skip",
        "hint",
        "collation",
        "fields",
        "encryptionInformation",
        "mirrored",
        "sampleId",
        "rawData",
        "includeQueryStatsMetrics",
    ];
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'count.{key}' is an unknown field."),
        ));
    }
    if let Some(problem) = read_option_problem(doc, "count") {
        return Err(problem);
    }
    if coll.starts_with('.') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Collection names cannot start with '.': {coll}"),
        ));
    }
    let limit = count_limit(doc.get("limit"))?;
    let skip = count_skip(doc.get("skip"));
    if skip < 0 {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("BSON field 'skip' value must be >= 0, actual value '{skip}'"),
        ));
    }
    argtypes::require_natural_hint(argtypes::given_hint(doc))?;
    // An undefined `$$variable` is a PARSE error (17276), not the storage
    // layer's generic "unsupported construct" BadValue.
    {
        let bound: Vec<String> = match doc.get("let") {
            Some(Bson::Document(d)) => d.keys().cloned().collect(),
            _ => Vec::new(),
        };
        if let Some(Bson::Document(f)) = doc.get("query") {
            if let Some((code, msg)) = argtypes::expression_problem_in_filter(f, &bound) {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    msg,
                ));
            }
        }
    }
    let storage = ctx.storage()?;
    let filter = doc_field(doc, "query");
    // A `$text` count of a collection that does not exist is 0.
    if storage
        .collection_exists(&ctx.db_name, &coll)
        .unwrap_or(false)
    {
        if let Some((code, msg)) = text_query_problem(storage, &ctx.db_name, &coll, &filter) {
            return Err(CommandError::new(
                code,
                crate::util::error_code_name(code),
                msg,
            ));
        }
    }
    let collation = crate::util::effective_collation(storage, &ctx.db_name, &coll, doc)?;
    let _active = secantus_core::collation::activate(collation.as_ref());

    // View support: a collection created with `viewOn` is a read-only view; its
    // count runs the view's pipeline (plus the count's query as a trailing
    // `$match`) through the aggregation engine. estimatedDocumentCount (the
    // drivers' "estimatedDocumentCount works correctly on views" tests) leans on
    // this. Mirrors `commands.py::_count`.
    let view_on = storage
        .get_collection_options(&ctx.db_name, &coll)
        .ok()
        .and_then(|o| o.get_str("viewOn").ok().map(String::from));
    let mut n = if view_on.is_some() {
        // Run on the VIEW: the aggregation resolves it, through any views
        // beneath it, and a view may hold a stage only a view may hold.
        let view_on = coll.clone();
        let mut pipeline: Vec<Bson> = Vec::new();
        if !filter.is_empty() {
            pipeline.push(Bson::Document(doc! { "$match": filter.clone() }));
        }
        pipeline.push(Bson::Document(doc! { "$count": "n" }));
        // Under the VIEW's collation, said outright so the base collection's
        // default does not stand in for it.
        let view_collation = match &collation {
            Some(c) => c.spec().clone(),
            None => doc! { "locale": "simple" },
        };
        let agg = doc! {
            "aggregate": view_on,
            "pipeline": pipeline,
            "cursor": {},
            "collation": view_collation,
        };
        let reply = crate::aggregate::aggregate(&agg, ctx)?;
        reply
            .get_document("cursor")
            .ok()
            .and_then(|c| c.get_array("firstBatch").ok())
            .and_then(|b| b.first())
            .and_then(Bson::as_document)
            .and_then(|d| d.get("n").and_then(as_i64))
            .unwrap_or(0)
    } else if let Some(hint) = argtypes::given_hint(doc) {
        // A `hint` forces a specific index; for an empty filter this still walks
        // that index, so hinting a sparse index counts only the docs present in
        // it (php-lib Count `testHintOption`). Count via the hinted find, which
        // honours the hint + sparse semantics. Mirrors `commands.py::_count`.
        storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &filter,
                None,
                Some(hint),
                collation.as_ref(),
                &Document::new(),
            )
            .map_err(command_error)?
            .len() as i64
    } else {
        // A `$near` needs its index whether or not the order matters.
        if let Some(near) = crate::near::find_near(&filter).map_err(command_error)? {
            near.check_index(storage, &ctx.db_name, &coll, Default::default())
                .map_err(command_error)?;
        }
        storage
            .count_collated(&ctx.db_name, &coll, &filter, collation.as_ref())
            .map_err(command_error)? as i64
    };
    if skip > 0 {
        n = n.saturating_sub(skip).max(0);
    }
    // A negative limit counts as its size (mongod reads it as the legacy
    // "single batch" spelling of a limit).
    let limit = limit.abs();
    if limit > 0 {
        n = n.min(limit);
    }
    Ok(doc! { "n": n as i32, "ok": 1.0 })
}

/// Stages allowed in a pipeline-form (`u: [...]`) update
/// (`update.py::_PIPELINE_UPDATE_STAGES`).
const PIPELINE_UPDATE_STAGES: [&str; 6] = [
    "$set",
    "$addFields",
    "$unset",
    "$project",
    "$replaceRoot",
    "$replaceWith",
];

/// Parse-time check of an operator-form update document, mirroring
/// `secantus.update.validate_update_doc`: `Some((code, errmsg))` for an unknown
/// modifier (9), an empty update path (56), or a path conflict (40), else `None`
/// (a pure replacement or a valid operator update).
///
/// This used to check ONLY the operator names, from a private copy of the
/// modifier list, and the caller hard-coded code 9. mongod interleaves all three
/// checks in one document-order walk and the first offender wins -- so a spec
/// with both an unknown modifier and an empty path answers whichever comes
/// first. `secantus_core::update::update_spec_error` is that walk, shared with
/// the engine so the two cannot drift.
fn update_spec_error(u: &Document) -> Option<(i32, String)> {
    secantus_core::update::update_spec_error(u)
}

/// `update` — batch update, one entry per `{q, u, multi?, upsert?}` spec. Ports
/// `commands.py::_update`. Document-, replacement-, and pipeline-form `u` all
/// apply (pipeline via `Storage::update_matching_pipeline`, diff-style oplog so
/// change streams see `operationType: "update"`). A malformed pipeline still
/// returns the faithful command-level `FailedToParse` (9) / `InvalidPipelineOperator`
/// (168).
///
/// Positional update operators (`$` / `$[]` / `$[ident]`) + `arrayFilters` apply
/// via `Storage::update_matching_array_filters` (`$` resolved from the query
/// filter, `$[ident]` from the per-statement `arrayFilters`); `let` + `collation`
/// thread through (collation forces a COLLSCAN match).
///
/// The collection `validator` is enforced on the post-apply document (read once
/// here, threaded into `Storage::update_matching`); malformed `writeConcern` is
/// rejected in `dispatch` before this handler. **Deferred:**
/// `_reject_oplog_rs_write` (writes to `local.oplog.rs`).
/// What mongod refuses about the `update` command's own fields before it runs
/// any statement: an unknown field, `updates` that is not an array of
/// documents, an unknown field of a statement. Measured 8.2.11, 2026-10-12;
/// each of these was accepted, and the field ignored.
fn update_command_problem(doc: &Document) -> Option<CommandError> {
    const FIELDS: &[&str] = &[
        "update",
        "updates",
        "ordered",
        "bypassDocumentValidation",
        "let",
        "stmtIds",
        "encryptionInformation",
        "bypassEmptyTsReplacement",
        "runtimeConstants",
        "legacyRuntimeConstants",
        "collectionUUID",
        "originalQuery",
        "originalCollation",
        "sampleId",
        "rawData",
    ];
    const STATEMENT_FIELDS: &[&str] = &[
        "q",
        "u",
        "c",
        "multi",
        "upsert",
        "upsertSupplied",
        "collation",
        "arrayFilters",
        "hint",
        "sort",
        "sampleId",
    ];
    let wrong_type = |path: String, value: &Bson, expected: &str| {
        CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
                secantus_core::query::bson_type_name(value)
            ),
        )
    };
    let missing = |path: &str| {
        CommandError::new(
            40414,
            "IDLFailedToParse",
            format!("BSON field '{path}' is missing but a required field"),
        )
    };
    let unknown = |path: String| {
        CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field '{path}' is an unknown field."),
        )
    };
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Some(unknown(format!("update.{key}")));
    }
    if let Some(problem) = empty_ts_flag_problem(doc) {
        return Some(problem);
    }
    if let Err(e) = argtypes::require_bool_or_number(
        doc,
        "bypassDocumentValidation",
        "update.bypassDocumentValidation",
    ) {
        return Some(e);
    }
    if matches!(doc.get("isTimeseriesNamespace"), Some(Bson::Boolean(true))) {
        return Some(CommandError::new(
            5916400,
            "Location5916400",
            "'isTimeseriesNamespace' parameter can only be set when the request is sent on \
             system.buckets namespace",
        ));
    }
    if matches!(doc.get("encryptionInformation"), Some(Bson::Document(e)) if !e.contains_key("schema"))
    {
        return Some(missing("update.encryptionInformation.schema"));
    }
    let statements = match doc.get("updates") {
        None | Some(Bson::Null) => return Some(missing("update.updates")),
        Some(Bson::Array(statements)) => statements,
        Some(other) => return Some(wrong_type("update.updates".to_string(), other, "array")),
    };
    for (index, statement) in statements.iter().enumerate() {
        let Bson::Document(statement) = statement else {
            return Some(wrong_type(
                format!("update.updates.{index}"),
                statement,
                "object",
            ));
        };
        if let Some(key) = statement
            .keys()
            .find(|k| !k.starts_with('$') && !STATEMENT_FIELDS.contains(&k.as_str()))
        {
            return Some(unknown(format!("update.updates.{key}")));
        }
    }
    None
}

pub fn update(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    update_in(doc, &ctx.db_name, ctx.storage()?)
}

/// [`update`] against a named database: `$merge` writes its documents with
/// it, as mongod writes them with the update command.
pub(crate) fn update_in(
    doc: &Document,
    db_name: &str,
    storage: &dyn crate::Storage,
) -> HandlerResult {
    argtypes::require_object(doc, "let", "update.let")?;
    argtypes::require_bool_field(doc, "ordered", "update.ordered")?;
    if let Some(problem) = update_command_problem(doc) {
        return Err(problem);
    }
    empty_batch(doc, "updates")?;
    let coll = coll_arg(doc, "update")?;
    if let Some(e) = crate::admin::invalid_write_namespace(db_name, &coll) {
        return Ok(e.into_reply());
    }
    let updates = array_field(doc, "updates");
    // NOTE: an unresolvable `hint` is a PER-STATEMENT writeError, not a
    // command-level failure. mongod answers `ok: 1` with
    // `writeErrors: [{index, code: 2}]` and applies the other statements in the
    // batch; returning `Err` here failed the whole command and reported no
    // `n`. Probed 8.2.11 (2026-08-31) against a bogus index name on both
    // `delete` and `update`. Validation therefore lives in the loop below,
    // beside the other per-statement checks, and not in a pre-pass.
    let ordered = bool_field(doc, "ordered", true);

    let mut n = 0_i32;
    let mut n_modified = 0_i32;
    let mut upserted: Vec<Bson> = Vec::new();
    let mut write_errors: Vec<Bson> = Vec::new();

    // Command-level `let` → vars visible to `$expr` in each spec's `q` filter
    // (and to pipeline-form `u` expressions). Resolved once for the batch.
    let let_vars = resolve_let_vars(doc.get("let"));

    // Collection validator on the POST-APPLY document (code 121) unless
    // `bypassDocumentValidation` or `validationAction: warn|off`. The validator
    // is read once here (no storage lock held) and threaded into the storage
    // update so it can check the rewritten doc before writing.
    let opts = storage
        .get_collection_options(db_name, &coll)
        .map_err(command_error)?;
    let validator = if bool_field(doc, "bypassDocumentValidation", false) {
        None
    } else {
        let action = opts.get_str("validationAction").unwrap_or("error");
        // `validationLevel: "off"` disables validation entirely, whatever the
        // action says — mongod checks the level first. The level was stored by
        // `create` / `collMod` and then never consulted, so a collection
        // explicitly opted OUT of validation still had it enforced.
        let level_off = opts.get_str("validationLevel").unwrap_or("strict") == "off";
        if level_off || action == "warn" || action == "off" {
            None
        } else {
            opts.get("validator").and_then(Bson::as_document).cloned()
        }
    };
    // `validationLevel: "moderate"` applies the validator to inserts and to
    // updates of documents that ALREADY satisfy it, but exempts documents that
    // were already invalid when the validator was introduced — the level exists
    // so a validator can be added to a collection with legacy rows without
    // freezing them.
    let validator_moderate = opts.get_str("validationLevel").unwrap_or("strict") == "moderate";

    // `Some` for a time-series collection, whose updates are restricted.
    let ts_fields = crate::timeseries::Fields::of(&opts);

    // What is wrong with `let`, if anything. mongod reports it against each
    // statement, not the command.
    let let_problem = match doc.get("let") {
        Some(Bson::Document(vars)) => argtypes::let_problem(vars),
        _ => None,
    };

    for (index, spec) in updates.iter().enumerate() {
        let Bson::Document(spec) = spec else { continue };

        argtypes::require_write_statement(spec, "update")?;
        if let Some((code, errmsg)) = &let_problem {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32, "code": *code, "errmsg": errmsg.clone(),
            }));
            if ordered {
                break;
            }
            continue;
        }
        // Strict bool, unlike findAndModify's `upsert`, which takes a bool OR any
        // number. Adjacent slots, different rules -- probed, not inferred.
        argtypes::require_bool_field(spec, "multi", "update.updates.multi")?;
        argtypes::require_bool_field(spec, "upsert", "update.updates.upsert")?;
        argtypes::require_object(spec, "collation", "update.updates.collation")?;
        argtypes::require_array_of_objects(spec, "arrayFilters", "update.updates.arrayFilters")?;
        argtypes::require_hint(spec, "hint")?;
        argtypes::require_object(spec, "c", "update.updates.c")?;

        // A time-series collection takes a multi update of its meta field
        // and no other; mongod says which rule a statement breaks before it
        // looks at the hint.
        if let Some(problem) = ts_fields
            .as_ref()
            .and_then(|f| crate::timeseries::update_problem(spec, f))
        {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": 72,
                "errmsg": problem,
            }));
            if ordered {
                break;
            }
            continue;
        }
        // An unresolvable `hint` fails THIS statement, not the batch (see the
        // note above the loop).
        if let Err(e) = crate::util::validate_write_hint(storage, db_name, &coll, spec.get("hint"))
        {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": e.code,
                "errmsg": e.errmsg.clone(),
            }));
            if ordered {
                break;
            }
            continue;
        }

        // Per-statement constants (`c`): more variables, for a PIPELINE update
        // only. They were accepted and ignored, so `$$k` was an undefined
        // variable (measured 8.2.11, 2026-10-10).
        let constants = spec.get("c").and_then(Bson::as_document);
        if constants.is_some() && !matches!(spec.get("u"), Some(Bson::Array(_))) {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": 51198,
                "errmsg": "Constant values may only be specified for pipeline updates",
            }));
            if ordered {
                break;
            }
            continue;
        }
        let stmt_vars: std::borrow::Cow<Document> = match constants {
            Some(c) => {
                let mut vars = let_vars.clone();
                for (name, value) in c {
                    vars.insert(name.clone(), value.clone());
                }
                std::borrow::Cow::Owned(vars)
            }
            None => std::borrow::Cow::Borrowed(&let_vars),
        };
        // `upsertSupplied`: when nothing matches, insert `c.new` as it stands
        // instead of running the pipeline over the filter's fields.
        let supplied: Option<Document> =
            if bool_field(spec, "upsert", false) && bool_field(spec, "upsertSupplied", false) {
                constants.and_then(|c| c.get_document("new").ok()).cloned()
            } else {
                None
            };

        // MongoDB 8.0's per-spec `sort`: match in sort order and update the
        // FIRST one. Probed on 8.2.11 -- `multi: true` is rejected, and an
        // upsert whose filter matches nothing still upserts. This used to
        // reject `sort` outright because we advertised 7.0; the driver `*-sort`
        // tests assert BOTH directions (`maxServerVersion: "7.99"` for the
        // rejection, `minServerVersion: "8.0"` for the support), so it moved
        // with the advertised version. Mirrors `_update` in
        // `src/secantus/commands.py`.
        let sort_spec = match spec.get("sort") {
            Some(Bson::Document(d)) => Some(d.clone()),
            None => None,
            Some(_) => {
                return Ok(CommandError::new(
                    2,
                    "BadValue",
                    "BSON field 'update.updates.sort' is the wrong type",
                )
                .into_reply());
            }
        };
        if sort_spec.is_some() && bool_field(spec, "multi", false) {
            return Ok(CommandError::new(
                9,
                "FailedToParse",
                "Cannot specify sort with multi=true",
            )
            .into_reply());
        }

        // `collation` is per-update-statement (inside each `updates[]` entry),
        // not a command-level field — collation-aware filter matching (COLLSCAN).
        let collation = match crate::util::effective_collation(storage, db_name, &coll, spec) {
            Ok(c) => c,
            Err(e) => {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32,
                    "code": e.code,
                    "errmsg": e.errmsg.clone(),
                }));
                if ordered {
                    break;
                }
                continue;
            }
        };
        let _active = secantus_core::collation::activate(collation.as_ref());
        let mut q = doc_field(spec, "q");
        if let Some(sort) = &sort_spec {
            // Resolve WHICH document sorts first, then pin the update to it.
            // Nothing matched -> leave the filter alone, so an upsert still
            // upserts.
            let sorted = Ok::<_, CommandError>(storage).and_then(|st| {
                crate::explain::order::find(
                    st,
                    &crate::explain::order::Ask {
                        db: db_name,
                        coll: &coll,
                        filter: &q,
                        sort: Some(sort),
                        hint: None,
                        collation: collation.as_ref(),
                        vars: &Document::new(),
                        limit: 1,
                        asked: Default::default(),
                    },
                )
                .map_err(|_| CommandError::new(2, "BadValue", "sort resolution failed"))
            });
            if let Ok(rows) = sorted {
                if let Some(first) = rows.first() {
                    if let Ok(d) = secantus_core::bsondoc::from_bytes(first.as_slice()) {
                        if let Some(id) = d.get("_id") {
                            q = doc! { "_id": id.clone() };
                        }
                    }
                }
            }
        }
        let multi = bool_field(spec, "multi", false);
        let upsert = bool_field(spec, "upsert", false);
        // A replacement document cannot apply to many documents. mongod refuses
        // the statement (measured 8.2.11, 2026-10-01); this applied it.
        let is_replacement_doc = matches!(spec.get("u"),
            Some(Bson::Document(u)) if !u.keys().any(|k| k.starts_with('$')));
        if multi && is_replacement_doc {
            write_errors.push(Bson::Document(doc! {
                "index": index as i32,
                "code": 9,
                "errmsg": "multi update is not supported for replacement-style update",
            }));
            if ordered {
                break;
            }
            continue;
        }

        // An undefined `$$variable` in the filter or in a PIPELINE-form `u` is a
        // per-statement writeError with mongod's 17276, not the storage layer's
        // generic BadValue — and it is per statement, so an earlier one in the
        // batch still applies (probed: `n: 1` with the error at `index: 1`).
        {
            let mut bound: Vec<String> = match doc.get("let") {
                Some(Bson::Document(d)) => d.keys().cloned().collect(),
                _ => Vec::new(),
            };
            if let Some(c) = constants {
                bound.extend(c.keys().cloned());
            }
            let found = match spec.get("q") {
                Some(Bson::Document(q)) => argtypes::expression_problem_in_filter(q, &bound)
                    .map(|(c, m)| (c, m, String::new())),
                _ => None,
            }
            .or_else(|| {
                spec.get("u")
                    .and_then(|u| argtypes::expression_problem_in_update(u, &bound))
            });
            if let Some((code, msg, stage)) = found {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32,
                    "code": code,
                    "errmsg": argtypes::wrap_expression_problem(&msg, &stage),
                }));
                if ordered {
                    break;
                }
                continue;
            }
        }

        // Parse-time validation of an operator-form `u`: an unknown modifier (or
        // mixing operators with replacement fields) is rejected at parse time —
        // mongod errors even against an empty / no-match collection, where the
        // apply-time engine would never run. Pipeline-form `u` is validated below.
        if !matches!(spec.get("u"), Some(Bson::Array(_))) {
            if let Some((code, errmsg)) = update_spec_error(&doc_field(spec, "u")) {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32, "code": code, "errmsg": errmsg,
                }));
                if ordered {
                    break;
                }
                continue;
            }
        }

        // An update that cannot insert leaves a collection that does not exist
        // alone. It used to create it (measured 8.2.11, 2026-10-12: the
        // collection was then listed).
        // The statement is still read first: a malformed one is refused.
        let absent = !upsert && !storage.collection_exists(db_name, &coll).unwrap_or(true);

        // Pipeline-form `u` (`[...]`) vs operator/replacement-form (`{...}`).
        // An update of ONE document changes the first its query matches in
        // the order mongod reads them.
        let first = if multi || absent {
            None
        } else {
            crate::explain::order::write_candidates(
                storage,
                &crate::explain::order::Ask {
                    db: db_name,
                    coll: &coll,
                    filter: &q,
                    sort: None,
                    hint: argtypes::given_hint(spec),
                    collation: collation.as_ref(),
                    vars: &stmt_vars,
                    limit: 0,
                    asked: Default::default(),
                },
            )
            .unwrap_or(None)
        };
        let outcome = if let Some(Bson::Array(stages)) = spec.get("u") {
            // Validate the shape up-front — real mongod returns a command-level
            // error for a malformed/unknown stage, not a per-op writeError.
            // A stage mongod does not know, or one with no name or two, fails
            // the command. A stage it knows but does not run in an update
            // fails the statement (measured 8.2.11, 2026-10-12).
            let mut refused: Option<(i32, String)> = None;
            for stage in stages {
                match stage {
                    Bson::Document(s) if s.len() == 1 => {
                        let name = s.keys().next().map(String::as_str).unwrap_or("");
                        if !crate::aggregate::recognized_stage(name) {
                            return Ok(CommandError::new(
                                40324,
                                "Location40324",
                                format!("Unrecognized pipeline stage name: '{name}'"),
                            )
                            .into_reply());
                        }
                        if !PIPELINE_UPDATE_STAGES.contains(&name) && refused.is_none() {
                            refused = Some((
                                72,
                                format!("{name} is not allowed to be used within an update"),
                            ));
                        }
                    }
                    _ => {
                        return Ok(CommandError::new(
                            40323,
                            "Location40323",
                            "A pipeline stage specification object must contain exactly one \
                             field.",
                        )
                        .into_reply())
                    }
                }
            }
            // What a stage holds is checked as `aggregate` checks it, and
            // reported against the statement.
            if refused.is_none() {
                refused = argtypes::stage_spec_error(stages).or_else(|| {
                    crate::aggregate::validate_unknown_exprs(stages)
                        .err()
                        .map(|e| (e.code, e.errmsg.clone()))
                });
            }
            if refused.is_none() && spec.get("arrayFilters").is_some_and(|f| f != &Bson::Null) {
                refused = Some((
                    9,
                    "arrayFilters may not be specified for pipeline-style updates".to_string(),
                ));
            }
            if let Some((code, errmsg)) = refused {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32, "code": code, "errmsg": errmsg,
                }));
                if ordered {
                    break;
                }
                continue;
            }
            if absent {
                continue;
            }
            crate::explain::order::pinned(storage, first, || {
                storage.update_matching_pipeline(
                    db_name,
                    &coll,
                    &q,
                    stages,
                    multi,
                    upsert && supplied.is_none(),
                    &stmt_vars,
                    collation.as_ref(),
                    validator.as_ref(),
                    validator_moderate,
                    false,
                )
            })
        } else {
            let mut u = doc_field(spec, "u");
            if is_replacement_doc {
                if let Some(filled) = fill_empty_timestamps(&u, storage) {
                    u = filled;
                }
            }
            // arrayFilters (`$[ident]`) is per-update-statement. The `$` / `$[]`
            // positional operators resolve in storage regardless; `$[ident]`
            // needs these filter docs.
            let array_filters: Vec<Document> = spec
                .get("arrayFilters")
                .and_then(Bson::as_array)
                .map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect())
                .unwrap_or_default();
            // PER-STATEMENT, not command-level: mongod reports this in
            // `writeErrors` with `ok: 1`, so an unordered batch's other
            // statements still apply. It used to fail the whole command, which
            // a driver sees as a different exception class entirely
            // (`OperationFailure` rather than `WriteError`).
            if let Some(e) = argtypes::array_filters_problem(&array_filters)
                .or_else(|| argtypes::array_filter_identifier_error(&u, &array_filters))
            {
                write_errors.push(Bson::Document(doc! {
                    "index": index as i32,
                    "code": e.code,
                    "errmsg": e.errmsg.clone(),
                }));
                if ordered {
                    break;
                }
                continue;
            }
            if absent {
                continue;
            }
            crate::explain::order::pinned(storage, first, || {
                storage.update_matching_array_filters(
                    db_name,
                    &coll,
                    &q,
                    &u,
                    multi,
                    upsert,
                    &array_filters,
                    &stmt_vars,
                    collation.as_ref(),
                    validator.as_ref(),
                    validator_moderate,
                    false,
                )
            })
        };

        match outcome {
            Ok(outcome) => {
                n += outcome.matched as i32;
                n_modified += outcome.modified as i32;
                if outcome.modified > 0 {
                    crate::transactions::note_statement(
                        index,
                        crate::transactions::StmtRecord::Update { upserted: None },
                    );
                }
                let mut upserted_id = outcome.upserted_id;
                if let (0, Some(mut new)) = (outcome.matched, supplied) {
                    // The `_id` is the query's, when it names one by equality,
                    // and `c.new` may not give another (measured 8.2.11,
                    // 2026-10-14: `{_id: 1}` with `new: {v: 5}` stores
                    // `{_id: 1, v: 5}`; with `new: {_id: 7}` it is 66).
                    let queried = query_id_equality(&q);
                    match (new.get("_id"), &queried) {
                        (None, Some(id)) => {
                            new.insert("_id", id.clone());
                        }
                        (None, None) => {
                            new.insert("_id", bson::oid::ObjectId::new());
                        }
                        (Some(given), Some(id))
                            if !secantus_core::update::same_id_value(given, id) =>
                        {
                            write_errors.push(Bson::Document(doc! {
                                "index": index as i32,
                                "code": 66,
                                "errmsg": format!(
                                    "Plan executor error during update :: caused by :: After \
                                     applying the update, the (immutable) field '_id' was found \
                                     to have been altered to _id: {}",
                                    secantus_core::update::render_shell(given)
                                ),
                            }));
                            if ordered {
                                break;
                            }
                            continue;
                        }
                        (Some(_), _) => {}
                    }
                    let id = new.get("_id").cloned().unwrap_or(Bson::Null);
                    // The supplied document is inserted as it stands, and is
                    // held to the validator like any other. It was not.
                    let refused = validator.as_ref().filter(|v| {
                        !secantus_core::query::matches(&new, v, &Document::new(), None)
                            .unwrap_or(true)
                    });
                    if let Some(v) = refused {
                        write_errors.push(Bson::Document(doc! {
                            "index": index as i32,
                            "code": 121,
                            "errmsg": "Plan executor error during update :: caused by :: \
                                       Document failed validation",
                            "errInfo": validation_error_info(v, &new),
                        }));
                        if ordered {
                            break;
                        }
                        continue;
                    }
                    let (_, mut errors) = storage
                        .insert(db_name, &coll, vec![encode_doc(&new)?], true)
                        .map_err(command_error)?;
                    if let Some(mut err) = errors.pop() {
                        err.insert("index", index as i32);
                        write_errors.push(Bson::Document(err));
                        if ordered {
                            break;
                        }
                        continue;
                    }
                    upserted_id = Some(id);
                }
                if let Some(id) = upserted_id {
                    crate::transactions::note_statement(
                        index,
                        crate::transactions::StmtRecord::Update {
                            upserted: Some(id.clone()),
                        },
                    );
                    upserted.push(Bson::Document(doc! { "index": index as i32, "_id": id }));
                    n += 1;
                }
            }
            Err(StorageError::Internal(msg)) => {
                return Ok(CommandError::new(1, "InternalError", msg).into_reply())
            }
            // A write conflict fails the whole statement command-level (mongod
            // semantics), so the dispatch txn envelope attaches the transient
            // label and drivers retry the transaction.
            Err(e @ StorageError::WriteConflict) => return Ok(command_error(e).into_reply()),
            Err(e) => {
                let failing = match &e {
                    StorageError::ValidationFailure(d) => Some(d.clone()),
                    _ => None,
                };
                let mut we = write_error(index, e, "update");
                if let (Some(d), Some(v)) = (failing, &validator) {
                    we.insert("errInfo", validation_error_info(v, &d));
                }
                write_errors.push(Bson::Document(we));
                if ordered {
                    break;
                }
            }
        }
    }

    let mut reply = doc! { "n": n, "nModified": n_modified, "ok": 1.0 };
    if !upserted.is_empty() {
        reply.insert("upserted", upserted);
    }
    if !write_errors.is_empty() {
        reply.insert("writeErrors", write_errors);
    }
    Ok(reply)
}

// --- helpers -------------------------------------------------------------

/// An array-valued field as a slice, or empty.
fn array_field<'a>(doc: &'a Document, key: &str) -> &'a [Bson] {
    match doc.get(key) {
        Some(Bson::Array(a)) => a,
        _ => &[],
    }
}

/// Encode a document to BSON bytes for the storage seam.
fn encode_doc(d: &Document) -> Result<Vec<u8>, CommandError> {
    let mut bytes = Vec::new();
    d.to_writer(&mut bytes).map_err(|e| {
        CommandError::new(
            1,
            "InternalError",
            format!("failed to encode document: {e}"),
        )
    })?;
    Ok(bytes)
}

fn bson_array(docs: Vec<Document>) -> Vec<Bson> {
    docs.into_iter().map(Bson::Document).collect()
}

/// The `_id` a query names by equality: `{_id: v}` or `{_id: {$eq: v}}`.
fn query_id_equality(q: &Document) -> Option<Bson> {
    match q.get("_id")? {
        Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => {
            if d.len() == 1 {
                d.get("$eq").cloned()
            } else {
                None
            }
        }
        v => Some(v.clone()),
    }
}

//! Small helpers shared across command handlers (field extraction, the byte
//! seam, and storage-error → reply mapping). Kept `pub(crate)`.

use bson::{doc, Bson, Document};

use crate::{CommandError, StorageError};

/// The collection name from a string-valued command field (`doc[cmd]`).
pub(crate) fn coll_arg(doc: &Document, cmd: &str) -> Result<String, CommandError> {
    match doc.get(cmd) {
        Some(Bson::String(s)) => Ok(s.clone()),
        _ => Err(CommandError::new(
            2,
            "BadValue",
            format!("{cmd} command requires a string collection name"),
        )),
    }
}

/// A document-valued field, or an empty document when absent / wrong type
/// (mirrors Python's `spec.get("q", {})` / `doc.get("query") or {}`).
pub(crate) fn doc_field(doc: &Document, key: &str) -> Document {
    doc.get(key)
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default()
}

/// A bool-valued field with a default (truthy like Python's `bool(...)`).
pub(crate) fn bool_field(doc: &Document, key: &str, default: bool) -> bool {
    doc.get(key).and_then(Bson::as_bool).unwrap_or(default)
}

/// Coerce any BSON number to `i64` (for `skip` / `limit` / `batchSize` / `index`).
pub(crate) fn as_i64(b: &Bson) -> Option<i64> {
    match b {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        _ => None,
    }
}

/// Decode a batch of encoded documents into reply `Bson::Document`s (for
/// `firstBatch` / `nextBatch`).
pub(crate) fn docs_to_bson(batch: Vec<Vec<u8>>) -> Result<Vec<Bson>, CommandError> {
    batch
        .iter()
        .map(|b| {
            bson::Document::from_reader(&mut b.as_slice())
                .map(Bson::Document)
                .map_err(|e| {
                    CommandError::new(
                        1,
                        "InternalError",
                        format!("failed to decode document: {e}"),
                    )
                })
        })
        .collect()
}

/// Shape a per-operation `writeError` document from a pre-classified storage
/// error (used by `delete`; `update` will reuse it).
pub(crate) fn write_error(index: usize, err: StorageError) -> Document {
    match err {
        StorageError::DuplicateKey(info) => {
            let mut e = doc! { "index": index as i32, "code": 11000, "errmsg": info.errmsg };
            if let Some(kp) = info.key_pattern {
                e.insert("keyPattern", kp);
            }
            if let Some(kv) = info.key_value {
                e.insert("keyValue", kv);
            }
            e
        }
        StorageError::WriteError { code, errmsg } => {
            doc! { "index": index as i32, "code": code, "errmsg": errmsg }
        }
        // Internal is handled by callers (command-level error); shouldn't reach
        // here, but degrade gracefully to a generic write error if it does.
        StorageError::Internal(msg) => {
            doc! { "index": index as i32, "code": 1, "errmsg": msg }
        }
    }
}

/// Map a storage error to a command-level `CommandError` (for non-batch
/// commands like `count` / `find`).
pub(crate) fn command_error(err: StorageError) -> CommandError {
    match err {
        StorageError::Internal(msg) => CommandError::new(1, "InternalError", msg),
        StorageError::WriteError { code, errmsg } => {
            CommandError::new(code, code_name_for(code), errmsg)
        }
        StorageError::DuplicateKey(info) => CommandError::new(11000, "DuplicateKey", info.errmsg),
    }
}

fn code_name_for(code: i32) -> &'static str {
    match code {
        2 => "BadValue",
        9 => "FailedToParse",
        11000 => "DuplicateKey",
        66 => "ImmutableField",
        _ => "Location",
    }
}

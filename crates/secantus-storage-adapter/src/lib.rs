//! `secantus-storage-adapter` — R4b: bridge the WiredTiger-backed
//! `secantus_storage::Storage` to the `secantus_commands::Storage` trait the
//! Rust server (R4a) dispatches against.
//!
//! The command crate stays WiredTiger-free by depending only on the trait; this
//! crate — which *does* link WiredTiger (via `secantus-storage`) and is therefore
//! excluded from the clean workspace and CI-validated only — supplies the
//! concrete implementation. It is a near-identity over the matching method
//! signatures, plus two translations:
//!
//! * **Hints:** the command seam passes the raw `hint` as `Bson`
//!   (`RawHint`); here it becomes a `secantus_storage::Hint`
//!   (`String` ⇒ `Name`, document ⇒ `KeySpec`).
//! * **Errors:** `secantus_storage::StorageError` → `secantus_commands::
//!   StorageError`, so duplicate keys keep their `keyPattern`/`keyValue`, bad
//!   hints / unsupported query constructs become a `BadValue` (2) write error,
//!   and engine/IO faults become `Internal`.

use std::sync::Arc;

use bson::{Bson, Document};
use secantus_commands::storage::{
    DuplicateKey, RawHint, Storage as CmdStorage, StorageError, UpdateOutcome,
};
use secantus_storage::{Hint, Storage as WtStorage, StorageError as WtError};

/// Wraps a shared WiredTiger-backed `Storage` and presents it as the command
/// layer's `Storage`. Construct with [`StorageAdapter::new`] and hand the
/// resulting `Arc<dyn secantus_commands::Storage>` to `secantus_server::bind`.
pub struct StorageAdapter {
    inner: Arc<WtStorage>,
}

impl StorageAdapter {
    pub fn new(inner: Arc<WtStorage>) -> Self {
        StorageAdapter { inner }
    }
}

impl CmdStorage for StorageAdapter {
    fn insert(
        &self,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError> {
        // Same return shape (inserted_count, write_error docs) on both sides.
        self.inner.insert(db, coll, docs, ordered).map_err(map_err)
    }

    fn update_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        let o = self
            .inner
            .update_matching(db, coll, filter, update, multi, upsert)
            .map_err(map_err)?;
        Ok(UpdateOutcome {
            matched: o.matched,
            modified: o.modified,
            upserted_id: o.upserted_id,
        })
    }

    fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
    ) -> Result<usize, StorageError> {
        self.inner
            .delete_matching(db, coll, filter, limit)
            .map_err(map_err)
    }

    fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<usize, StorageError> {
        self.inner.count_matching(db, coll, filter).map_err(map_err)
    }

    fn find(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let resolved = hint.map(to_hint);
        self.inner
            .find_matching_with(db, coll, filter, sort, resolved.as_ref())
            .map_err(map_err)
    }

    fn list_collections(&self, db: &str) -> Result<Vec<String>, StorageError> {
        self.inner.list_collections(db).map_err(map_err)
    }

    fn create_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.create_collection(db, coll).map_err(map_err)
    }

    fn drop_collection(&self, db: &str, coll: &str) -> Result<bool, StorageError> {
        self.inner.drop_collection(db, coll).map_err(map_err)
    }

    fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>, StorageError> {
        self.inner.list_indexes(db, coll).map_err(map_err)
    }

    fn create_index(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        key: &Document,
        options: &Document,
    ) -> Result<bool, StorageError> {
        self.inner
            .create_index(db, coll, name, key, options)
            .map_err(map_err)
    }

    fn drop_index(&self, db: &str, coll: &str, name: &str) -> Result<bool, StorageError> {
        self.inner.drop_index(db, coll, name).map_err(map_err)
    }

    fn drop_all_indexes(&self, db: &str, coll: &str) -> Result<usize, StorageError> {
        self.inner.drop_all_indexes(db, coll).map_err(map_err)
    }
}

/// Convert a raw `hint` value into the storage `Hint`. A string is an index
/// name (or `"$natural"` / `"_id_"`); a document is a key spec. Anything else
/// falls through to an empty name, which `resolve_hint` rejects as `BadHint`
/// (→ `BadValue` at the command layer), matching mongod.
fn to_hint(b: RawHint<'_>) -> Hint {
    match b {
        Bson::String(s) => Hint::Name(s.clone()),
        Bson::Document(d) => Hint::KeySpec(d.clone()),
        _ => Hint::Name(String::new()),
    }
}

/// Translate a storage error into the command layer's pre-classified error.
fn map_err(e: WtError) -> StorageError {
    match e {
        WtError::DuplicateKey(conflict) => StorageError::DuplicateKey(Box::new(DuplicateKey {
            errmsg: format!(
                "E11000 duplicate key error index: {} dup key: {:?}",
                conflict.index, conflict.key_value
            ),
            key_pattern: Some(conflict.key_pattern),
            key_value: Some(conflict.key_value),
        })),
        WtError::DuplicateId => StorageError::WriteError {
            code: 11000,
            errmsg: "E11000 duplicate key error".to_string(),
        },
        // Bad hint / unsupported query construct → BadValue (2), the same code
        // the Python server surfaces for these at the command layer.
        WtError::BadHint(m) => StorageError::WriteError { code: 2, errmsg: m },
        WtError::QueryUnsupported => StorageError::WriteError {
            code: 2,
            errmsg: "query uses a construct the Rust server does not support".to_string(),
        },
        WtError::UnsupportedId => StorageError::WriteError {
            code: 2,
            errmsg: "_id is of a type the Rust server does not support".to_string(),
        },
        WtError::UnsupportedValue => StorageError::WriteError {
            code: 2,
            errmsg: "an indexed value is of a type the Rust server does not support".to_string(),
        },
        // Index-create / change-stream faults don't arise on the CRUD path, but
        // map them to a command-level internal error if they ever surface here.
        WtError::CreateIndexUnsupported(m)
        | WtError::IndexOptionsConflict(m)
        | WtError::ChangeStreamFatal(m) => StorageError::Internal(m),
        WtError::Wt(err) => StorageError::Internal(format!("WiredTiger error: {err:?}")),
        WtError::Bson(m) => StorageError::Internal(format!("BSON error: {m}")),
    }
}

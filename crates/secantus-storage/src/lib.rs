//! SecantusDB's storage layer, in Rust — Phase 4 sub-phase 1 (the CRUD core).
//!
//! This is the first vertical of the storage keystone: the collections and
//! documents tables (`secantus_collections` / `secantus_documents`) and their
//! CRUD operations (insert, find-by-`_id`,
//! natural-order scan, replace, delete, collection registry), built on
//! `secantus-wt` (the WiredTiger FFI) and `secantus-core`'s `sortkey` (the
//! byte-sortable `id_key` encoding). It mirrors the behaviour of the relevant
//! slice of `src/secantus/storage.py`:
//!
//! * documents live at `(db, coll, id_key) -> bson(doc)` where `id_key =
//!   sortkey.encode_value(_id)` — so iterating the table yields MongoDB's
//!   cross-type natural order;
//! * inserts use a non-overwriting cursor so a duplicate `_id` surfaces as a
//!   duplicate-key error;
//! * a global lock serialises public methods (1:1 with `storage.py`'s `RLock`).
//!
//! Later sub-phases add indexes, geo, and the oplog (see
//! `tasks/rust-rewrite-phase4-scoping.md`).

use std::sync::Mutex;

use bson::oid::ObjectId;
use bson::{Bson, Document};
use secantus_core::sortkey;
use secantus_wt::{Connection, Cursor, Session, WtError};

const COLL_TABLE: &str = "table:secantus_collections";
const DOC_TABLE: &str = "table:secantus_documents";

/// The WiredTiger connection config SecantusDB uses (mirrors `storage.py`):
/// logging on, commit-sync off by default.
const DEFAULT_CONFIG: &str = "create,session_max=1000,cache_size=256M,\
                              log=(enabled=true,file_max=10MB),\
                              transaction_sync=(enabled=false,method=fsync)";

// The full table set `storage.py` bootstraps. Sub-phase 1 only reads/writes the
// collections + documents tables, but creating the rest keeps the on-disk schema
// identical so later sub-phases don't need a migration.
const BOOTSTRAP: &[(&str, &str)] = &[
    (COLL_TABLE, "key_format=SS,value_format=u"),
    (DOC_TABLE, "key_format=SSu,value_format=u"),
    ("table:secantus_indexes", "key_format=SSS,value_format=u"),
    (
        "table:secantus_index_entries",
        "key_format=SSSu,value_format=u",
    ),
    ("table:secantus_oplog", "key_format=q,value_format=u"),
    ("table:secantus_preimages", "key_format=q,value_format=u"),
    ("table:secantus_oplog_meta", "key_format=S,value_format=u"),
    ("table:secantus_users", "key_format=SS,value_format=u"),
    ("table:secantus_roles", "key_format=SS,value_format=u"),
    (
        "table:secantus_profile_settings",
        "key_format=S,value_format=u",
    ),
];

#[derive(Debug)]
pub enum StorageError {
    Wt(WtError),
    /// A document could not be BSON-decoded / encoded.
    Bson(String),
    /// `_id` is a type the sort-key encoder doesn't handle.
    UnsupportedId,
    /// A document was inserted with an `_id` that already exists.
    DuplicateId,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Wt(e) => write!(f, "{e}"),
            StorageError::Bson(m) => write!(f, "BSON error: {m}"),
            StorageError::UnsupportedId => write!(f, "unsupported _id type for sort-key encoding"),
            StorageError::DuplicateId => write!(f, "duplicate _id"),
        }
    }
}
impl std::error::Error for StorageError {}
impl From<WtError> for StorageError {
    fn from(e: WtError) -> Self {
        StorageError::Wt(e)
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

fn encode_doc(doc: &Document) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    doc.to_writer(&mut buf)
        .map_err(|e| StorageError::Bson(e.to_string()))?;
    Ok(buf)
}

fn decode_doc(bytes: &[u8]) -> Result<Document> {
    Document::from_reader(&mut std::io::Cursor::new(bytes))
        .map_err(|e| StorageError::Bson(e.to_string()))
}

/// `id_key = sortkey.encode_value(_id)` — the byte-sortable key for the `_id`.
fn id_key(id: &Bson) -> Result<Vec<u8>> {
    sortkey::encode_value(id, None).map_err(|_| StorageError::UnsupportedId)
}

/// WiredTiger-backed storage. A global lock serialises public methods, matching
/// `storage.py`'s serialize-everything `RLock` discipline (the WiredTiger C-level
/// concurrency story is unchanged; see `tasks/wt-bindings-plan.md`).
pub struct Storage {
    conn: Connection,
    lock: Mutex<()>,
}

impl Storage {
    /// Open (creating if needed) an on-disk database at `home` with the default
    /// SecantusDB WiredTiger config, bootstrapping the table schema.
    pub fn open(home: &str) -> Result<Storage> {
        Self::open_with_config(home, DEFAULT_CONFIG)
    }

    /// Open with an explicit WiredTiger config (e.g. add `in_memory=true` for an
    /// ephemeral database).
    pub fn open_with_config(home: &str, config: &str) -> Result<Storage> {
        let conn = Connection::open(home, config)?;
        {
            let boot = conn.open_session()?;
            for (name, fmt) in BOOTSTRAP {
                boot.create(name, fmt)?;
            }
        }
        Ok(Storage {
            conn,
            lock: Mutex::new(()),
        })
    }

    /// Insert one BSON-encoded document. Assigns an `ObjectId` `_id` if absent.
    /// Returns the document's `id_key`. A duplicate `_id` yields
    /// `StorageError::DuplicateId`.
    pub fn insert_one(&self, db: &str, coll: &str, doc_bytes: &[u8]) -> Result<Vec<u8>> {
        let _g = self.lock.lock().unwrap();
        let mut doc = decode_doc(doc_bytes)?;
        if !doc.contains_key("_id") {
            doc.insert("_id", Bson::ObjectId(ObjectId::new()));
        }
        let id = doc.get("_id").expect("_id present").clone();
        let key = id_key(&id)?;
        let blob = encode_doc(&doc)?;

        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;
        // overwrite=false -> a pre-existing _id is a WT_DUPLICATE_KEY.
        let cur = session.open_cursor(DOC_TABLE, Some("overwrite=false"))?;
        cur.set_key_ssu(db, coll, &key);
        cur.set_value_u(&blob);
        match cur.insert() {
            Ok(()) => Ok(key),
            Err(e) if e.is_duplicate_key() => Err(StorageError::DuplicateId),
            Err(e) => Err(e.into()),
        }
    }

    /// Fetch a document by `_id`. Returns its BSON bytes, or `None`.
    pub fn find_by_id(&self, db: &str, coll: &str, id: &Bson) -> Result<Option<Vec<u8>>> {
        let _g = self.lock.lock().unwrap();
        let key = id_key(id)?;
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        match cur.search() {
            Ok(()) => Ok(Some(cur.get_value_u()?)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// All documents of a collection in natural (`_id`) order, as BSON bytes.
    pub fn scan_collection(&self, db: &str, coll: &str) -> Result<Vec<Vec<u8>>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        let mut out = Vec::new();
        // Position at the first key >= (db, coll, "") then walk while the
        // (db, coll) prefix matches.
        cur.set_key_ssu(db, coll, b"");
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false, // empty table
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, _id) = cur.get_key_ssu()?;
            if d != db || c != coll {
                break;
            }
            out.push(cur.get_value_u()?);
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Replace the document at `id` with `new_doc_bytes` (whose `_id` is forced to
    /// `id`, matching `storage.py`'s replacement semantics). Returns `false` if no
    /// document had that `_id`.
    pub fn replace_by_id(
        &self,
        db: &str,
        coll: &str,
        id: &Bson,
        new_doc_bytes: &[u8],
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        let key = id_key(id)?;
        let session = self.conn.open_session()?;

        // Existence check.
        let probe = session.open_cursor(DOC_TABLE, None)?;
        probe.set_key_ssu(db, coll, &key);
        match probe.search() {
            Ok(()) => {}
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        }

        let mut doc = decode_doc(new_doc_bytes)?;
        doc.insert("_id", id.clone()); // replacement preserves _id
        let blob = encode_doc(&doc)?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        cur.set_value_u(&blob);
        cur.update()?;
        Ok(true)
    }

    /// Delete the document with `_id == id`. Returns `false` if absent.
    pub fn delete_by_id(&self, db: &str, coll: &str, id: &Bson) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        let key = id_key(id)?;
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        match cur.remove() {
            Ok(()) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn collection_exists(&self, db: &str, coll: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(COLL_TABLE, None)?;
        cur.set_key_ss(db, coll);
        match cur.search() {
            Ok(()) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Collection names registered under `db`, in registry order.
    pub fn list_collections(&self, db: &str) -> Result<Vec<String>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(COLL_TABLE, None)?;
        let mut out = Vec::new();
        cur.set_key_ss(db, "");
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c) = cur.get_key_ss()?;
            if d != db {
                break;
            }
            out.push(c);
            more = cur.next()?;
        }
        Ok(out)
    }
}

/// Register `(db, coll)` in the collections table if not already present.
fn ensure_collection(session: &Session, db: &str, coll: &str) -> Result<()> {
    let probe = session.open_cursor(COLL_TABLE, None)?;
    probe.set_key_ss(db, coll);
    match probe.search() {
        Ok(()) => Ok(()),
        Err(e) if e.is_not_found() => {
            let cur: Cursor = session.open_cursor(COLL_TABLE, None)?;
            // `opts` must outlive `insert()`: set_value_u stores a pointer to the
            // bytes (the WT_ITEM borrow-until-op contract), so an inline
            // temporary would be freed before WiredTiger reads it.
            let opts = empty_options();
            cur.set_key_ss(db, coll);
            cur.set_value_u(&opts);
            cur.insert()?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// An empty options document (`{}`) as BSON bytes — the collections-table value.
fn empty_options() -> Vec<u8> {
    encode_doc(&Document::new()).expect("encoding an empty document cannot fail")
}

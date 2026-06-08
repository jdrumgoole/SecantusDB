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

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

use bson::oid::ObjectId;
use bson::{Bson, Document};
use secantus_core::get_path;
use secantus_core::query::matches as query_matches;
use secantus_core::sortkey::{self, COMPOUND_SEP};
use secantus_wt::{Connection, Cursor, Session, WtError};

const COLL_TABLE: &str = "table:secantus_collections";
const DOC_TABLE: &str = "table:secantus_documents";
const IDX_TABLE: &str = "table:secantus_indexes";
const IDX_ENTRIES_TABLE: &str = "table:secantus_index_entries";

/// The synthetic `_id` index name. The `_id_` index is virtual — never stored
/// in the registry; `list_indexes` synthesises it.
const ID_INDEX_NAME: &str = "_id_";

/// The entry-key separator between the (escaped) index sort-key bytes and the
/// document's `id_key`. Mirrors `storage._ENTRY_SEP`.
const ENTRY_SEP: &[u8] = b"\x00\x00";

/// Index options whose value conflicting with an existing index of the same
/// name makes `create_index` reject the re-creation (mirrors `storage.py`).
const CONFLICTING_OPTS: &[&str] = &[
    "unique",
    "sparse",
    "hidden",
    "expireAfterSeconds",
    "partialFilterExpression",
];

/// Field-level operators a single-field index can serve. Mirrors
/// `storage._RANGE_OPS`.
const RANGE_OPS: &[&str] = &["$eq", "$gt", "$gte", "$lt", "$lte", "$in"];

/// The plan `find_matching` would use for a filter — what `explain_plan`
/// reports. Mirrors `storage.explain_plan`'s `{kind, index_name, key_pattern,
/// direction}` shape.
#[derive(Debug, Clone, PartialEq)]
pub enum ExplainPlan {
    /// A full collection scan.
    CollScan,
    /// An index scan over `index_name` (`key_pattern`), walked in `direction`
    /// (`"forward"` / `"backward"`; always `"forward"` until sort acceleration
    /// lands in slice 2f).
    IxScan {
        index_name: String,
        key_pattern: Document,
        direction: String,
    },
}

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
    /// An indexed field value couldn't be sort-key encoded (e.g. a construct
    /// the Rust encoder defers to Python, like a regex or a collation edge).
    UnsupportedValue,
    /// A document was inserted with an `_id` that already exists.
    DuplicateId,
    /// `create_index` was asked for an index type the Rust storage engine
    /// doesn't implement yet (text / hashed / geo).
    CreateIndexUnsupported(String),
    /// `create_index` was asked to re-create an existing index with conflicting
    /// options.
    IndexOptionsConflict(String),
    /// A query filter used a construct the Rust query engine can't evaluate
    /// (the `matches` "defer to Python" signal). The server's engine selection
    /// is responsible for not routing such queries to the Rust storage.
    QueryUnsupported,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Wt(e) => write!(f, "{e}"),
            StorageError::Bson(m) => write!(f, "BSON error: {m}"),
            StorageError::UnsupportedId => write!(f, "unsupported _id type for sort-key encoding"),
            StorageError::UnsupportedValue => {
                write!(f, "unsupported value type for index sort-key encoding")
            }
            StorageError::DuplicateId => write!(f, "duplicate _id"),
            StorageError::CreateIndexUnsupported(m) => write!(f, "{m}"),
            StorageError::IndexOptionsConflict(m) => write!(f, "{m}"),
            StorageError::QueryUnsupported => {
                write!(f, "query construct not supported by the Rust query engine")
            }
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

// --- index-key construction (mirrors `storage.py`, byte-for-byte) ---

/// The per-field sort direction of a `key_spec` value: `Some(1)`/`Some(-1)` for
/// numeric directions, `None` for non-numeric specs (geo `"2dsphere"`/`"2d"`,
/// `"text"`, `"hashed"` — not supported by the Rust engine yet).
fn direction_of(v: &Bson) -> Option<i32> {
    match v {
        Bson::Int32(i) => Some(*i),
        Bson::Int64(i) => Some(*i as i32),
        Bson::Double(d) => Some(*d as i32),
        _ => None,
    }
}

/// Direction-aware sort-key encoding for one value (defers to Python on the
/// constructs the Rust encoder can't reproduce).
fn enc_dir(v: &Bson, direction: i32) -> Result<Vec<u8>> {
    sortkey::encode_value_directed(v, direction, None).map_err(|_| StorageError::UnsupportedValue)
}

/// Order-preserving escape so `\x00\x00` is unambiguous as a separator: every
/// `0x00` byte becomes `0x00 0xff`. Mirrors `storage._escape_kb`.
fn escape_kb(kb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(kb.len());
    for &b in kb {
        out.push(b);
        if b == 0 {
            out.push(0xff);
        }
    }
    out
}

/// Pack an index-entry payload into a single trailing `u` column:
/// `escape(kb) + b"\x00\x00" + id_key`. WiredTiger length-prefixes non-trailing
/// `u` columns, which would break lexicographic order — so both halves live in
/// one column and the B-tree sorts by `escape(kb)` first, then `id_key`.
/// Mirrors `storage._pack_entry`.
fn pack_entry(kb: &[u8], id_key: &[u8]) -> Vec<u8> {
    let mut out = escape_kb(kb);
    out.extend_from_slice(ENTRY_SEP);
    out.extend_from_slice(id_key);
    out
}

/// Split a packed entry into `(escaped_kb, id_key)` at the FIRST `\x00\x00`.
/// Correct because the `kb` half is escaped (no bare `\x00\x00` can occur in
/// it). Mirrors `storage._unpack_entry`.
fn unpack_entry(packed: &[u8]) -> (&[u8], &[u8]) {
    match packed.windows(2).position(|w| w == ENTRY_SEP) {
        Some(i) => (&packed[..i], &packed[i + 2..]),
        None => (packed, &[]),
    }
}

/// Join compound key parts with `COMPOUND_SEP` between components (mirrors
/// Python's `COMPOUND_SEP.join(parts)`).
fn compound_join(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(COMPOUND_SEP);
        }
        out.extend_from_slice(p);
    }
    out
}

/// True if any field of `key_spec` resolves to an array value in `doc` — the
/// signal that marks an index multikey. Mirrors `storage._doc_makes_multikey`.
fn doc_makes_multikey(doc: &Document, key_spec: &Document) -> bool {
    key_spec
        .keys()
        .any(|f| matches!(get_path(doc, f), Some(Bson::Array(_))))
}

/// All byte-keys `doc` contributes to an index under `key_spec`. Scalars give
/// one key; arrays give one key per (deduped) element *plus* the whole-array
/// key (the multikey layout); compound indexes take the cartesian product
/// across each field's candidate values. Missing fields encode as `null`
/// (non-sparse semantics — sparse handling is a later slice). Mirrors
/// `storage._index_key_variants`.
fn index_key_variants(doc: &Document, key_spec: &Document) -> Result<Vec<Vec<u8>>> {
    let fields: Vec<(&String, i32)> = key_spec
        .iter()
        .map(|(k, v)| (k, direction_of(v).unwrap_or(1)))
        .collect();

    // Per-field candidate values: scalars -> [val]; arrays -> [uniq elems..., whole_array].
    let mut per_field: Vec<Vec<Bson>> = Vec::with_capacity(fields.len());
    for (f, d) in &fields {
        let v = get_path(doc, f).cloned().unwrap_or(Bson::Null);
        if let Bson::Array(arr) = &v {
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            let mut uniq: Vec<Bson> = Vec::new();
            for elem in arr {
                let eb = enc_dir(elem, *d)?;
                if seen.insert(eb) {
                    uniq.push(elem.clone());
                }
            }
            uniq.push(v.clone()); // whole-array key
            per_field.push(uniq);
        } else {
            per_field.push(vec![v]);
        }
    }

    if fields.len() == 1 {
        let d = fields[0].1;
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for val in &per_field[0] {
            let kb = enc_dir(val, d)?;
            if seen.insert(kb.clone()) {
                keys.push(kb);
            }
        }
        return Ok(keys);
    }

    // Compound: cartesian product across the per-field candidate lists.
    let mut combos: Vec<Vec<&Bson>> = vec![Vec::new()];
    for cand in &per_field {
        let mut next: Vec<Vec<&Bson>> = Vec::with_capacity(combos.len() * cand.len());
        for combo in &combos {
            for v in cand {
                let mut c = combo.clone();
                c.push(v);
                next.push(c);
            }
        }
        combos = next;
    }
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for combo in &combos {
        let mut parts: Vec<Vec<u8>> = Vec::with_capacity(fields.len());
        for (i, (_f, d)) in fields.iter().enumerate() {
            parts.push(enc_dir(combo[i], *d)?);
        }
        let kb = compound_join(&parts);
        if seen.insert(kb.clone()) {
            keys.push(kb);
        }
    }
    Ok(keys)
}

/// True for a BSON regular-expression value (never a point-lookup target).
fn is_regex_value(v: &Bson) -> bool {
    matches!(v, Bson::RegularExpression(_))
}

/// Flip a range operator for a DESC field (whose stored bytes are inverted, so
/// the comparison reverses). Non-range ops pass through. Mirrors the
/// `{$gt:$lt, ...}` table in `storage.py`.
fn flip_range_op(op: &str) -> &str {
    match op {
        "$gt" => "$lt",
        "$gte" => "$lte",
        "$lt" => "$gt",
        "$lte" => "$gte",
        other => other,
    }
}

/// The `id_key`s to fetch for an `{_id: <spec>}` equality predicate, or `None`
/// when `spec` isn't a pure point lookup (range op, regex, literal subdocument,
/// operator-valued equality). The documents table is keyed by
/// `encode_value(_id)`, so `_id` equality is a primary-key point lookup, not a
/// COLLSCAN — and `_id_` is virtual (no entries table). `$in` keys come back
/// deduplicated in ascending byte order. Mirrors `storage._id_point_lookup_keys`.
fn id_point_lookup_keys(spec: &Bson) -> Result<Option<Vec<Vec<u8>>>> {
    match spec {
        Bson::Document(d) => {
            let keys: Vec<&String> = d.keys().collect();
            if keys.is_empty() || !keys.iter().all(|k| k.starts_with('$')) {
                return Ok(None); // literal subdocument _id — normal path
            }
            if keys.len() == 1 && keys[0] == "$eq" {
                let v = d.get("$eq").unwrap();
                if matches!(v, Bson::Document(_)) || is_regex_value(v) {
                    return Ok(None);
                }
                return Ok(Some(vec![id_key(v)?]));
            }
            if keys.len() == 1 && keys[0] == "$in" {
                let vals = match d.get("$in") {
                    Some(Bson::Array(a)) => a,
                    _ => return Ok(None),
                };
                if vals
                    .iter()
                    .any(|v| matches!(v, Bson::Document(_)) || is_regex_value(v))
                {
                    return Ok(None);
                }
                let mut set: BTreeSet<Vec<u8>> = BTreeSet::new();
                for v in vals {
                    set.insert(id_key(v)?);
                }
                return Ok(Some(set.into_iter().collect()));
            }
            Ok(None)
        }
        _ if is_regex_value(spec) => Ok(None),
        _ => Ok(Some(vec![id_key(spec)?])),
    }
}

/// Split a filter into `(eq_fields, operator_field, operator_ops)` for the
/// compound prefix + trailing-operator shape: any number of bare-equality
/// fields plus exactly one operator-form field whose ops are all in
/// `RANGE_OPS`. `None` if it doesn't fit. Mirrors
/// `storage._partition_compound_range_filter`.
fn partition_compound_range_filter(filter: &Document) -> Option<(Document, String, Document)> {
    let mut eq_fields = Document::new();
    let mut operator_field: Option<String> = None;
    let mut operator_ops: Option<Document> = None;
    for (f, v) in filter {
        if let Bson::Document(opd) = v {
            if opd.is_empty() || !opd.keys().all(|k| k.starts_with('$')) {
                return None;
            }
            if !opd.keys().all(|k| RANGE_OPS.contains(&k.as_str())) {
                return None;
            }
            if operator_field.is_some() {
                return None;
            }
            operator_field = Some(f.clone());
            operator_ops = Some(opd.clone());
        } else {
            eq_fields.insert(f.clone(), v.clone());
        }
    }
    let of = operator_field?;
    if eq_fields.is_empty() || eq_fields.contains_key(&of) {
        return None;
    }
    Some((eq_fields, of, operator_ops.unwrap_or_default()))
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
            Ok(()) => {}
            Err(e) if e.is_duplicate_key() => return Err(StorageError::DuplicateId),
            Err(e) => return Err(e.into()),
        }
        // Maintain secondary indexes: write this doc's entries.
        let indexes = self.collection_indexes(&session, db, coll)?;
        self.write_index_entries(&session, db, coll, &doc, &indexes)?;
        Ok(key)
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

        // Existence check — capture the old doc so we can retract its entries.
        let probe = session.open_cursor(DOC_TABLE, None)?;
        probe.set_key_ssu(db, coll, &key);
        let old_blob = match probe.search() {
            Ok(()) => probe.get_value_u()?,
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let old_doc = decode_doc(&old_blob)?;

        let mut doc = decode_doc(new_doc_bytes)?;
        doc.insert("_id", id.clone()); // replacement preserves _id
        let blob = encode_doc(&doc)?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        cur.set_value_u(&blob);
        cur.update()?;

        // Maintain secondary indexes: retract the old doc's entries, write the new.
        let indexes = self.collection_indexes(&session, db, coll)?;
        if !indexes.is_empty() {
            self.delete_index_entries(&session, db, coll, &old_doc, &indexes)?;
            self.write_index_entries(&session, db, coll, &doc, &indexes)?;
        }
        Ok(true)
    }

    /// Delete the document with `_id == id`. Returns `false` if absent.
    pub fn delete_by_id(&self, db: &str, coll: &str, id: &Bson) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        let key = id_key(id)?;
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        // Read the doc first so we can retract its index entries, then remove.
        let old_blob = match cur.search() {
            Ok(()) => cur.get_value_u()?,
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        cur.remove()?;
        let old_doc = decode_doc(&old_blob)?;
        let indexes = self.collection_indexes(&session, db, coll)?;
        self.delete_index_entries(&session, db, coll, &old_doc, &indexes)?;
        Ok(true)
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

    // --- secondary indexes (Phase 4 sub-phase 2) ---

    /// Create a secondary index `name` over `key_spec` (field → direction `1`/
    /// `-1`) with `options`. Builds entries by scanning the collection once.
    /// Returns `true` if created, `false` if an index of that name already
    /// exists with compatible options (or `name == "_id_"`). Rejects non-numeric
    /// index types (geo / text / hashed — deferred to later slices) with
    /// `CreateIndexUnsupported`, and re-creation with conflicting options with
    /// `IndexOptionsConflict`. Mirrors `storage.create_index`.
    pub fn create_index(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        key_spec: &Document,
        options: &Document,
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        if name == ID_INDEX_NAME {
            return Ok(false);
        }
        for (field, v) in key_spec {
            if direction_of(v).is_none() {
                let ty = match v {
                    Bson::String(s) => s.clone(),
                    other => format!("{other:?}"),
                };
                return Err(StorageError::CreateIndexUnsupported(format!(
                    "{ty} indexes (field {field:?}) are not supported by the Rust storage engine yet"
                )));
            }
        }

        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;

        let c = session.open_cursor(IDX_TABLE, None)?;
        c.set_key_sss(db, coll, name);
        match c.search() {
            Ok(()) => {
                // Index exists: reject conflicting options, else no-op success.
                let existing = decode_doc(&c.get_value_u()?)?;
                let existing_opts = existing.get_document("options").ok();
                for opt in CONFLICTING_OPTS {
                    let in_new = options.contains_key(opt);
                    let in_old = existing_opts.is_some_and(|o| o.contains_key(opt));
                    if (in_new || in_old)
                        && options.get(opt) != existing_opts.and_then(|o| o.get(opt))
                    {
                        return Err(StorageError::IndexOptionsConflict(format!(
                            "Index with name '{name}' already exists with different options"
                        )));
                    }
                }
                return Ok(false);
            }
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }

        // One doc-table walk: detect multikey and build all entry keys.
        let mut multikey = false;
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (id_k, blob) in self.scan_docs(&session, db, coll)? {
            let d = decode_doc(&blob)?;
            if !multikey && doc_makes_multikey(&d, key_spec) {
                multikey = true;
            }
            for kb in index_key_variants(&d, key_spec)? {
                entries.push((kb, id_k.clone()));
            }
        }

        let mut stored_options = options.clone();
        if multikey {
            stored_options.insert("multikey", Bson::Boolean(true));
        }
        let mut payload_doc = Document::new();
        payload_doc.insert("key", Bson::Document(key_spec.clone()));
        payload_doc.insert("options", Bson::Document(stored_options));
        let payload = encode_doc(&payload_doc)?;

        c.reset()?;
        c.set_key_sss(db, coll, name);
        c.set_value_u(&payload);
        c.insert()?;

        let ec = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (kb, id_k) in &entries {
            let packed = pack_entry(kb, id_k);
            ec.reset()?;
            ec.set_key_sssu(db, coll, name, &packed);
            ec.set_value_u(b"");
            ec.insert()?;
        }
        Ok(true)
    }

    /// All indexes on `(db, coll)` in MongoDB's `listIndexes` shape (the virtual
    /// `_id_` index first, then stored indexes), sorted by name. Empty when the
    /// collection doesn't exist. Mirrors `storage.list_indexes`.
    pub fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        if !collection_registered(&session, db, coll)? {
            return Ok(Vec::new());
        }
        let mut id_key_spec = Document::new();
        id_key_spec.insert("_id", 1i32);
        let mut id_idx = Document::new();
        id_idx.insert("v", 2i32);
        id_idx.insert("key", Bson::Document(id_key_spec));
        id_idx.insert("name", ID_INDEX_NAME.to_string());
        let mut out: Vec<Document> = vec![id_idx];
        for (name, key_spec, opts) in self.iter_indexes(&session, db, coll)? {
            let mut e = Document::new();
            e.insert("v", 2i32);
            e.insert("key", Bson::Document(key_spec));
            e.insert("name", name);
            for (k, v) in &opts {
                e.insert(k.clone(), v.clone());
            }
            out.push(e);
        }
        out.sort_by(|a, b| {
            a.get_str("name")
                .unwrap_or("")
                .cmp(b.get_str("name").unwrap_or(""))
        });
        Ok(out)
    }

    /// Drop the index named `name` (and all its entries). Returns `false` if no
    /// such index, or `name == "_id_"`. Mirrors `storage.drop_index`.
    pub fn drop_index(&self, db: &str, coll: &str, name: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        if name == ID_INDEX_NAME {
            return Ok(false);
        }
        let session = self.conn.open_session()?;
        let c = session.open_cursor(IDX_TABLE, None)?;
        c.set_key_sss(db, coll, name);
        match c.search() {
            Ok(()) => {}
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        c.remove()?;
        self.delete_entries_prefix(&session, db, coll, name)?;
        Ok(true)
    }

    /// Drop every (non-`_id_`) index on `(db, coll)`. Returns how many were
    /// dropped. Mirrors `storage.drop_all_indexes` (used by drop-collection).
    pub fn drop_all_indexes(&self, db: &str, coll: &str) -> Result<usize> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let names: Vec<String> = self
            .iter_indexes(&session, db, coll)?
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        for name in &names {
            let c = session.open_cursor(IDX_TABLE, None)?;
            c.set_key_sss(db, coll, name);
            if c.search().is_ok() {
                c.remove()?;
            }
            self.delete_entries_prefix(&session, db, coll, name)?;
        }
        Ok(names.len())
    }

    /// Walk the registry for `(db, coll)`: `(name, key_spec, options)` per index.
    fn iter_indexes(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
    ) -> Result<Vec<(String, Document, Document)>> {
        let cur = session.open_cursor(IDX_TABLE, None)?;
        let mut out = Vec::new();
        cur.set_key_sss(db, coll, "");
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
            let (d, c, name) = cur.get_key_sss()?;
            if d != db || c != coll {
                break;
            }
            let payload = decode_doc(&cur.get_value_u()?)?;
            let key_spec = payload.get_document("key").cloned().unwrap_or_default();
            let opts = payload.get_document("options").cloned().unwrap_or_default();
            out.push((name, key_spec, opts));
            more = cur.next()?;
        }
        Ok(out)
    }

    /// `(name, key_spec)` for every stored index — what the entry-maintenance
    /// paths need.
    fn collection_indexes(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
    ) -> Result<Vec<(String, Document)>> {
        Ok(self
            .iter_indexes(session, db, coll)?
            .into_iter()
            .map(|(n, k, _)| (n, k))
            .collect())
    }

    /// `(id_key, doc_bytes)` for every document in `(db, coll)`, natural order.
    fn scan_docs(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let cur = session.open_cursor(DOC_TABLE, None)?;
        let mut out = Vec::new();
        cur.set_key_ssu(db, coll, b"");
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
            let (d, c, idk) = cur.get_key_ssu()?;
            if d != db || c != coll {
                break;
            }
            out.push((idk, cur.get_value_u()?));
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Write `doc`'s index entries for every index in `indexes`.
    fn write_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        indexes: &[(String, Document)],
    ) -> Result<()> {
        if indexes.is_empty() {
            return Ok(());
        }
        let id = doc
            .get("_id")
            .ok_or_else(|| StorageError::Bson("document missing _id".into()))?;
        let id_k = id_key(id)?;
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (name, key_spec) in indexes {
            for kb in index_key_variants(doc, key_spec)? {
                let packed = pack_entry(&kb, &id_k);
                cur.reset()?;
                cur.set_key_sssu(db, coll, name, &packed);
                cur.set_value_u(b"");
                cur.insert()?;
            }
        }
        Ok(())
    }

    /// Remove `doc`'s index entries for every index in `indexes` (recomputes the
    /// same packed keys `write_index_entries` produced).
    fn delete_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        indexes: &[(String, Document)],
    ) -> Result<()> {
        if indexes.is_empty() {
            return Ok(());
        }
        let id = doc
            .get("_id")
            .ok_or_else(|| StorageError::Bson("document missing _id".into()))?;
        let id_k = id_key(id)?;
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (name, key_spec) in indexes {
            for kb in index_key_variants(doc, key_spec)? {
                let packed = pack_entry(&kb, &id_k);
                cur.reset()?;
                cur.set_key_sssu(db, coll, name, &packed);
                match cur.remove() {
                    Ok(()) => {}
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }

    /// Delete all entries for one index (its `(db, coll, name)` prefix).
    fn delete_entries_prefix(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<()> {
        let scan = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        scan.set_key_sssu(db, coll, name, b"");
        let mut packs: Vec<Vec<u8>> = Vec::new();
        let mut more = match scan.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    scan.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, n, packed) = scan.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            packs.push(packed);
            more = scan.next()?;
        }
        let del = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for p in &packs {
            del.reset()?;
            del.set_key_sssu(db, coll, name, p);
            match del.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Introspection: the entries of index `name` as `(escaped_kb, id_key)`
    /// pairs in WiredTiger (sorted) order. Primarily for tests and explain-style
    /// inspection; the lookup paths in later slices read entries directly.
    pub fn index_entries(
        &self,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let scan = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        scan.set_key_sssu(db, coll, name, b"");
        let mut out = Vec::new();
        let mut more = match scan.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    scan.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, n, packed) = scan.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            let (kb, idk) = unpack_entry(&packed);
            out.push((kb.to_vec(), idk.to_vec()));
            more = scan.next()?;
        }
        Ok(out)
    }

    // --- query routing (Phase 4 sub-phase 2, slice 2b: single-field) ---

    /// Documents matching `filter`, as BSON bytes. Routes through a single-field
    /// index (equality / `$eq` / `$in` / range) or the `_id` primary-key point
    /// lookup when one applies, else a full collection scan; index candidates
    /// are always re-checked with `matches()` (an index walk can over-include,
    /// e.g. multikey). Compound-index routing is slice 2c. Mirrors the no-sort
    /// path of `storage.find_matching`.
    pub fn find_matching(&self, db: &str, coll: &str, filter: &Document) -> Result<Vec<Vec<u8>>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let blobs = match self.try_index_id_keys(&session, db, coll, filter)? {
            Some(id_keys) => self.docs_by_id_keys(&session, db, coll, &id_keys)?,
            None => self
                .scan_docs(&session, db, coll)?
                .into_iter()
                .map(|(_id_k, blob)| blob)
                .collect(),
        };
        let vars = Document::new();
        let mut out = Vec::new();
        for blob in blobs {
            let d = decode_doc(&blob)?;
            if query_matches(&d, filter, &vars, None).map_err(|_| StorageError::QueryUnsupported)? {
                out.push(blob);
            }
        }
        Ok(out)
    }

    /// The plan `find_matching` would use for `filter` (no execution). Mirrors
    /// `storage.explain_plan` (filter only; sort / hint are slice 2f).
    pub fn explain_plan(&self, db: &str, coll: &str, filter: &Document) -> Result<ExplainPlan> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        match self.pick_index_for_filter(&session, db, coll, filter)? {
            Some((index_name, key_pattern)) => Ok(ExplainPlan::IxScan {
                index_name,
                key_pattern,
                direction: "forward".to_string(),
            }),
            None => Ok(ExplainPlan::CollScan),
        }
    }

    /// Route `filter` to a set of candidate `id_key`s via an index, or `None`
    /// (caller does a COLLSCAN). The `_id` point-lookup fast path, compound
    /// bare-equality prefix, compound prefix + trailing operator, and
    /// single-field equality / `$in` / range. Mirrors `storage._try_index_id_keys`
    /// (geo dispatch is sub-phase 3).
    fn try_index_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        if filter.is_empty() {
            return Ok(None);
        }
        if filter.keys().any(|f| f.starts_with('$')) {
            return Ok(None);
        }
        // `_id` equality is a primary-key point lookup on the documents table.
        if filter.len() == 1 {
            if let Some(spec) = filter.get("_id") {
                if let Some(id_keys) = id_point_lookup_keys(spec)? {
                    return Ok(Some(id_keys));
                }
            }
        }
        // (Geo dispatch -> sub-phase 3.) Bare-equality filters of any size can
        // use a compound (or single-field) index whose leading fields cover them.
        if filter.values().all(|v| !matches!(v, Bson::Document(_))) {
            if let Some(r) = self.try_compound_eq_id_keys(session, db, coll, filter)? {
                return Ok(Some(r));
            }
        }
        // Compound prefix + trailing operator field (eq fields then range / $in).
        if filter.len() >= 2 {
            if let Some(r) = self.try_compound_range_id_keys(session, db, coll, filter)? {
                return Ok(Some(r));
            }
        }
        if filter.len() != 1 {
            return Ok(None);
        }
        let (field, value) = filter.iter().next().unwrap();
        let idx = match self.find_leading_field_index(session, db, coll, field)? {
            Some(m) => m,
            None => return Ok(None),
        };
        self.lookup_id_keys_via_leading_field(session, db, coll, &idx, value)
    }

    /// The best index whose leading field is `field`, as `(name, direction,
    /// is_compound)`. Single-field indexes win over compound (tighter scan);
    /// otherwise the first compound index with that leading field is the
    /// fallback. Skips non-`1`/`-1` directions (geo / text / hashed). (Partial /
    /// collation gating is slice 2e.) Multikey indexes are NOT skipped —
    /// per-element entries cover the lookup, and `find_matching` re-checks with
    /// `matches()`. Mirrors `storage._find_leading_field_index`.
    fn find_leading_field_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        field: &str,
    ) -> Result<Option<(String, i32, bool)>> {
        let mut compound_fallback: Option<(String, i32, bool)> = None;
        for (name, key_spec, _opts) in self.iter_indexes(session, db, coll)? {
            let n_fields = key_spec.len();
            let (f0, _) = match key_spec.iter().next() {
                Some(p) => p,
                None => continue,
            };
            if f0.as_str() != field {
                continue;
            }
            if !key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let d = direction_of(key_spec.get(field).unwrap()).unwrap();
            if n_fields == 1 {
                return Ok(Some((name, d, false)));
            }
            if compound_fallback.is_none() {
                compound_fallback = Some((name, d, true));
            }
        }
        Ok(compound_fallback)
    }

    /// `id_key`s for `field <value>` against the index `(name, direction,
    /// is_compound)` whose leading field is `field`: bare/`$eq`/`$in` equality
    /// and `$gt`/`$gte`/`$lt`/`$lte` ranges (operator semantics flip for a DESC
    /// field). A compound index is walked by its leading field only (equality is
    /// a prefix scan; range uses the leading-field range scan). `None` falls back
    /// to COLLSCAN. Mirrors `storage._lookup_id_keys_via_leading_field`.
    fn lookup_id_keys_via_leading_field(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        idx: &(String, i32, bool),
        value: &Bson,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        let (name, direction, is_compound) = (idx.0.as_str(), idx.1, idx.2);
        let opdoc = match value {
            Bson::Document(d) => d,
            _ => {
                return Ok(Some(self.eq_id_keys(
                    session,
                    db,
                    coll,
                    name,
                    direction,
                    is_compound,
                    value,
                )?))
            }
        };
        if opdoc.is_empty() || !opdoc.keys().all(|k| k.starts_with('$')) {
            return Ok(None);
        }
        if !opdoc.keys().all(|k| RANGE_OPS.contains(&k.as_str())) {
            return Ok(None);
        }
        if opdoc.contains_key("$in") {
            if opdoc.len() != 1 {
                return Ok(None);
            }
            let vals = match opdoc.get("$in") {
                Some(Bson::Array(a)) => a,
                _ => return Ok(None),
            };
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            let mut out: Vec<Vec<u8>> = Vec::new();
            for v in vals {
                if matches!(v, Bson::Document(_)) {
                    return Ok(None);
                }
                for id_k in self.eq_id_keys(session, db, coll, name, direction, is_compound, v)? {
                    if seen.insert(id_k.clone()) {
                        out.push(id_k);
                    }
                }
            }
            return Ok(Some(out));
        }
        let mut lower: Option<Vec<u8>> = None;
        let mut lower_incl = true;
        let mut upper: Option<Vec<u8>> = None;
        let mut upper_incl = true;
        for (op, bound) in opdoc {
            if matches!(bound, Bson::Document(_)) {
                return Ok(None);
            }
            if op == "$eq" {
                return Ok(Some(self.eq_id_keys(
                    session,
                    db,
                    coll,
                    name,
                    direction,
                    is_compound,
                    bound,
                )?));
            }
            let kb = enc_dir(bound, direction)?;
            // DESC field: stored bytes are inverted, so the comparison flips.
            let eff = if direction == -1 {
                flip_range_op(op)
            } else {
                op.as_str()
            };
            match eff {
                "$gt" => (lower, lower_incl) = (Some(kb), false),
                "$gte" => (lower, lower_incl) = (Some(kb), true),
                "$lt" => (upper, upper_incl) = (Some(kb), false),
                "$lte" => (upper, upper_incl) = (Some(kb), true),
                _ => {}
            }
        }
        if is_compound {
            // Walk the compound index using its leading field only; boundary
            // detection accounts for the escaped compound separator.
            return Ok(Some(self.range_scan_index_leading(
                session,
                db,
                coll,
                name,
                lower.as_deref(),
                lower_incl,
                upper.as_deref(),
                upper_incl,
            )?));
        }
        Ok(Some(self.range_scan_index(
            session,
            db,
            coll,
            name,
            lower.as_deref(),
            lower_incl,
            upper.as_deref(),
            upper_incl,
            None,
        )?))
    }

    /// `id_key`s whose index entry equals `value` on the leading field: an exact
    /// `kb` scan for a single-field index, or a `kb + COMPOUND_SEP` prefix scan
    /// for a compound index. Mirrors `storage._eq_id_keys_via_leading`.
    #[allow(clippy::too_many_arguments)]
    fn eq_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
        direction: i32,
        is_compound: bool,
        value: &Bson,
    ) -> Result<Vec<Vec<u8>>> {
        let kb = enc_dir(value, direction)?;
        if is_compound {
            let mut seed = kb;
            seed.extend_from_slice(COMPOUND_SEP);
            self.scan_index_for_id_keys(session, db, coll, name, &seed, true)
        } else {
            self.scan_index_for_id_keys(session, db, coll, name, &kb, false)
        }
    }

    /// Walk index `name`'s entries matching `kb`: exact (`prefix=false`) or
    /// `escape(kb)`-prefixed (`prefix=true`). Mirrors
    /// `storage._scan_index_for_id_keys`.
    fn scan_index_for_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
        kb: &[u8],
        prefix: bool,
    ) -> Result<Vec<Vec<u8>>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        let esc_kb = escape_kb(kb);
        let seed = if prefix {
            esc_kb.clone()
        } else {
            let mut s = esc_kb.clone();
            s.extend_from_slice(ENTRY_SEP);
            s
        };
        cur.set_key_sssu(db, coll, name, &seed);
        let mut out = Vec::new();
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, n, packed) = cur.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            let (row_esc, row_id) = unpack_entry(&packed);
            if prefix {
                if !row_esc.starts_with(esc_kb.as_slice()) {
                    break;
                }
            } else if row_esc != esc_kb.as_slice() {
                break;
            }
            out.push(row_id.to_vec());
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Range-scan index `name` between optional `lower` / `upper` bounds (on the
    /// directed, unescaped `kb`). Optional `prefix` constrains the scan to
    /// entries whose escaped kb starts with `escape(prefix)` — used by compound
    /// prefix + trailing-operator queries where leading equalities pin part of
    /// the kb. Mirrors `storage._range_scan_index`.
    #[allow(clippy::too_many_arguments)]
    fn range_scan_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
        lower: Option<&[u8]>,
        lower_inclusive: bool,
        upper: Option<&[u8]>,
        upper_inclusive: bool,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<Vec<u8>>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        let esc_prefix = prefix.map(escape_kb);
        let esc_lower = lower.map(escape_kb);
        let esc_upper = upper.map(escape_kb);
        let seed: Vec<u8> = if let Some(el) = &esc_lower {
            let mut s = el.clone();
            s.extend_from_slice(ENTRY_SEP);
            s
        } else if let Some(ep) = &esc_prefix {
            ep.clone()
        } else {
            Vec::new()
        };
        cur.set_key_sssu(db, coll, name, &seed);
        let mut out = Vec::new();
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, n, packed) = cur.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            let (row_esc, row_id) = unpack_entry(&packed);
            if let Some(ep) = &esc_prefix {
                if !row_esc.starts_with(ep.as_slice()) {
                    break;
                }
            }
            // Exclusive lower: skip rows whose kb equals the lower bound.
            if let Some(el) = &esc_lower {
                if !lower_inclusive && row_esc == el.as_slice() {
                    more = cur.next()?;
                    continue;
                }
            }
            if let Some(eu) = &esc_upper {
                if upper_inclusive {
                    if row_esc > eu.as_slice() {
                        break;
                    }
                } else if row_esc >= eu.as_slice() {
                    break;
                }
            }
            out.push(row_id.to_vec());
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Range-scan a compound index using only its leading field. Each row's
    /// escaped kb is `escape(enc(leading)) + escape(SEP) + escape(enc(rest))`;
    /// boundary detection uses `starts_with(esc_X + escape(SEP))` to find rows
    /// whose leading field equals `X` (an escaped numeric terminator can overlap
    /// the escaped separator, so a literal split is unreliable). Mirrors
    /// `storage._range_scan_index_leading`.
    #[allow(clippy::too_many_arguments)]
    fn range_scan_index_leading(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
        lower: Option<&[u8]>,
        lower_inclusive: bool,
        upper: Option<&[u8]>,
        upper_inclusive: bool,
    ) -> Result<Vec<Vec<u8>>> {
        let esc_compound_sep = escape_kb(COMPOUND_SEP);
        let esc_lower = lower.map(escape_kb);
        let esc_upper = upper.map(escape_kb);
        let seed: Vec<u8> = esc_lower.clone().unwrap_or_default();
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        cur.set_key_sssu(db, coll, name, &seed);
        let mut out = Vec::new();
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        let eq_prefix = |b: &[u8]| -> Vec<u8> {
            let mut p = b.to_vec();
            p.extend_from_slice(&esc_compound_sep);
            p
        };
        let lower_eq_prefix = esc_lower.as_deref().map(eq_prefix);
        let upper_eq_prefix = esc_upper.as_deref().map(eq_prefix);
        while more {
            let (d, c, n, packed) = cur.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            let (row_esc, row_id) = unpack_entry(&packed);
            if let Some(lep) = &lower_eq_prefix {
                if !lower_inclusive && row_esc.starts_with(lep.as_slice()) {
                    more = cur.next()?;
                    continue;
                }
            }
            if let Some(eu) = &esc_upper {
                if upper_inclusive {
                    if row_esc > eu.as_slice()
                        && !row_esc.starts_with(upper_eq_prefix.as_ref().unwrap().as_slice())
                    {
                        break;
                    }
                } else if row_esc >= eu.as_slice() {
                    break;
                }
            }
            out.push(row_id.to_vec());
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Fetch documents by `id_key` (deduped, order-preserving — a multikey index
    /// can yield the same `id_key` more than once). Mirrors
    /// `storage._docs_by_id_keys`.
    fn docs_by_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        id_keys: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>> {
        let cur = session.open_cursor(DOC_TABLE, None)?;
        let mut seen: HashSet<&[u8]> = HashSet::new();
        let mut out = Vec::new();
        for id_k in id_keys {
            if !seen.insert(id_k.as_slice()) {
                continue;
            }
            cur.reset()?;
            cur.set_key_ssu(db, coll, id_k);
            match cur.search() {
                Ok(()) => out.push(cur.get_value_u()?),
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }

    /// The index `(name, key_spec)` `explain_plan` would report for `filter`, or
    /// `None` (COLLSCAN). Mirrors `storage._pick_index_for_filter` (no
    /// execution); the selection order matches `try_index_id_keys`.
    fn pick_index_for_filter(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<(String, Document)>> {
        if filter.is_empty() || filter.keys().any(|f| f.starts_with('$')) {
            return Ok(None);
        }
        if filter.len() == 1 {
            if let Some(spec) = filter.get("_id") {
                if id_point_lookup_keys(spec)?.is_some() {
                    let mut kp = Document::new();
                    kp.insert("_id", 1i32);
                    return Ok(Some((ID_INDEX_NAME.to_string(), kp)));
                }
            }
        }
        if filter.values().all(|v| !matches!(v, Bson::Document(_))) {
            if let Some(p) = self.pick_compound_eq_index(session, db, coll, filter)? {
                return Ok(Some(p));
            }
        }
        if filter.len() >= 2 {
            if let Some(p) = self.pick_compound_range_index(session, db, coll, filter)? {
                return Ok(Some(p));
            }
        }
        if filter.len() != 1 {
            return Ok(None);
        }
        let (field, value) = filter.iter().next().unwrap();
        let idx = match self.find_leading_field_index(session, db, coll, field)? {
            Some(m) => m,
            None => return Ok(None),
        };
        // Operator-form values must be range ops the index can serve.
        if let Bson::Document(opdoc) = value {
            if opdoc.is_empty()
                || !opdoc.keys().all(|k| k.starts_with('$'))
                || !opdoc.keys().all(|k| RANGE_OPS.contains(&k.as_str()))
            {
                return Ok(None);
            }
        }
        match self.key_spec_for(session, db, coll, &idx.0)? {
            Some(key_spec) => Ok(Some((idx.0, key_spec))),
            None => Ok(None),
        }
    }

    /// The stored `key_spec` of index `name`, or `None`.
    fn key_spec_for(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<Option<Document>> {
        for (n, key_spec, _opts) in self.iter_indexes(session, db, coll)? {
            if n == name {
                return Ok(Some(key_spec));
            }
        }
        Ok(None)
    }

    // --- compound-index routing (Phase 4 sub-phase 2, slice 2c) ---

    /// The index `try_compound_eq_id_keys` would walk for a bare-equality
    /// `filter`: one whose leading fields (set-wise) cover the filter's fields,
    /// preferring the shortest. `None` if none covers it. Mirrors
    /// `storage._pick_compound_eq_index`. (Partial / collation gating is 2e.)
    fn pick_compound_eq_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<(String, Document)>> {
        let filter_fields: HashSet<&str> = filter.keys().map(|s| s.as_str()).collect();
        let eff_len = filter_fields.len();
        let mut best: Option<(String, Document)> = None;
        for (name, key_spec, _opts) in self.iter_indexes(session, db, coll)? {
            if !key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let idx_fields: Vec<&String> = key_spec.keys().collect();
            if idx_fields.len() < eff_len {
                continue;
            }
            let prefix_set: HashSet<&str> =
                idx_fields[..eff_len].iter().map(|s| s.as_str()).collect();
            if prefix_set != filter_fields {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(_, b)| b.len() > idx_fields.len())
            {
                best = Some((name, key_spec.clone()));
            }
            if idx_fields.len() == eff_len {
                break;
            }
        }
        Ok(best)
    }

    /// Bare-equality filter against a compound (or single-field) index prefix:
    /// equality (full cover) or prefix (strict leading prefix) scan. `None`
    /// falls back to COLLSCAN. Mirrors `storage._try_compound_eq_id_keys`.
    fn try_compound_eq_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        let (name, key_spec) = match self.pick_compound_eq_index(session, db, coll, filter)? {
            Some(p) => p,
            None => return Ok(None),
        };
        let idx_fields: Vec<&String> = key_spec.keys().collect();
        // Index-order fields that the filter constrains (partial-filter clauses
        // live outside the key — not relevant until 2e).
        let prefix_fields: Vec<&String> = idx_fields
            .iter()
            .copied()
            .filter(|f| filter.contains_key(f.as_str()))
            .collect();
        let mut parts: Vec<Vec<u8>> = Vec::with_capacity(prefix_fields.len());
        for f in &prefix_fields {
            let dir = direction_of(key_spec.get(f.as_str()).unwrap()).unwrap();
            parts.push(enc_dir(filter.get(f.as_str()).unwrap(), dir)?);
        }
        let kb = compound_join(&parts);
        if prefix_fields.len() == idx_fields.len() {
            return Ok(Some(
                self.scan_index_for_id_keys(session, db, coll, &name, &kb, false)?,
            ));
        }
        let mut seed = kb;
        seed.extend_from_slice(COMPOUND_SEP);
        Ok(Some(self.scan_index_for_id_keys(
            session, db, coll, &name, &seed, true,
        )?))
    }

    /// The index `try_compound_range_id_keys` would walk: leading equalities
    /// (set-wise) then the operator field as the next column, shortest first.
    /// `None` if none fits. Mirrors `storage._pick_compound_range_index`.
    fn pick_compound_range_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<(String, Document)>> {
        let (eq_fields, operator_field, _ops) = match partition_compound_range_filter(filter) {
            Some(p) => p,
            None => return Ok(None),
        };
        let eq_set: HashSet<&str> = eq_fields.keys().map(|s| s.as_str()).collect();
        let target = eq_set.len();
        let mut best: Option<(String, Document)> = None;
        for (name, key_spec, _opts) in self.iter_indexes(session, db, coll)? {
            if !key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let idx_fields: Vec<&String> = key_spec.keys().collect();
            if idx_fields.len() <= target {
                continue;
            }
            let prefix_set: HashSet<&str> =
                idx_fields[..target].iter().map(|s| s.as_str()).collect();
            if prefix_set != eq_set {
                continue;
            }
            if idx_fields[target].as_str() != operator_field {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(_, b)| b.len() > idx_fields.len())
            {
                best = Some((name, key_spec.clone()));
            }
            if idx_fields.len() == target + 1 {
                break;
            }
        }
        Ok(best)
    }

    /// Compound-prefix lookup with a trailing operator field — `{a: 5, b: {$gt:
    /// 10}}`: pin the prefix from the leading equalities, apply the operator's
    /// `$eq` / `$in` / range bounds (DESC-flipped) to the next column. `None`
    /// falls back to COLLSCAN. Mirrors `storage._try_compound_range_id_keys`.
    fn try_compound_range_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        let (eq_fields, operator_field, operator_ops) =
            match partition_compound_range_filter(filter) {
                Some(p) => p,
                None => return Ok(None),
            };
        let (name, key_spec) = match self.pick_compound_range_index(session, db, coll, filter)? {
            Some(p) => p,
            None => return Ok(None),
        };
        let idx_fields: Vec<&String> = key_spec.keys().collect();
        let target = eq_fields.len();
        let op_dir = direction_of(key_spec.get(operator_field.as_str()).unwrap()).unwrap();
        let mut eq_parts: Vec<Vec<u8>> = Vec::with_capacity(target);
        for f in &idx_fields[..target] {
            let dir = direction_of(key_spec.get(f.as_str()).unwrap()).unwrap();
            eq_parts.push(enc_dir(eq_fields.get(f.as_str()).unwrap(), dir)?);
        }
        let mut prefix_with_sep = compound_join(&eq_parts);
        prefix_with_sep.extend_from_slice(COMPOUND_SEP);
        let use_prefix = idx_fields.len() > target + 1;

        // Helper: prefix + enc(value), then COMPOUND_SEP if more columns follow.
        let make_kb = |v: &Bson| -> Result<Vec<u8>> {
            let mut kb = prefix_with_sep.clone();
            kb.extend_from_slice(&enc_dir(v, op_dir)?);
            if use_prefix {
                kb.extend_from_slice(COMPOUND_SEP);
            }
            Ok(kb)
        };

        if operator_ops.contains_key("$in") {
            if operator_ops.len() != 1 {
                return Ok(None);
            }
            let vals = match operator_ops.get("$in") {
                Some(Bson::Array(a)) => a,
                _ => return Ok(None),
            };
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            let mut out: Vec<Vec<u8>> = Vec::new();
            for v in vals {
                if matches!(v, Bson::Document(_)) {
                    return Ok(None);
                }
                let inner = make_kb(v)?;
                for id_k in
                    self.scan_index_for_id_keys(session, db, coll, &name, &inner, use_prefix)?
                {
                    if seen.insert(id_k.clone()) {
                        out.push(id_k);
                    }
                }
            }
            return Ok(Some(out));
        }
        if operator_ops.contains_key("$eq") {
            if operator_ops.len() != 1 {
                return Ok(None);
            }
            let inner = make_kb(operator_ops.get("$eq").unwrap())?;
            return Ok(Some(self.scan_index_for_id_keys(
                session, db, coll, &name, &inner, use_prefix,
            )?));
        }
        let mut lower: Option<Vec<u8>> = None;
        let mut lower_incl = true;
        let mut upper: Option<Vec<u8>> = None;
        let mut upper_incl = true;
        for (op, bound) in &operator_ops {
            if matches!(bound, Bson::Document(_)) {
                return Ok(None);
            }
            let mut full = prefix_with_sep.clone();
            full.extend_from_slice(&enc_dir(bound, op_dir)?);
            let eff = if op_dir == -1 {
                flip_range_op(op)
            } else {
                op.as_str()
            };
            match eff {
                "$gt" => (lower, lower_incl) = (Some(full), false),
                "$gte" => (lower, lower_incl) = (Some(full), true),
                "$lt" => (upper, upper_incl) = (Some(full), false),
                "$lte" => (upper, upper_incl) = (Some(full), true),
                _ => return Ok(None),
            }
        }
        Ok(Some(self.range_scan_index(
            session,
            db,
            coll,
            &name,
            lower.as_deref(),
            lower_incl,
            upper.as_deref(),
            upper_incl,
            Some(&prefix_with_sep),
        )?))
    }
}

/// True if `(db, coll)` is registered in the collections table.
fn collection_registered(session: &Session, db: &str, coll: &str) -> Result<bool> {
    let cur = session.open_cursor(COLL_TABLE, None)?;
    cur.set_key_ss(db, coll);
    match cur.search() {
        Ok(()) => Ok(true),
        Err(e) if e.is_not_found() => Ok(false),
        Err(e) => Err(e.into()),
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

#[cfg(test)]
mod tests {
    //! Byte-exact unit tests for the pure index-key functions. These pin the
    //! on-disk entry layout to the Python reference (`storage._pack_entry` /
    //! `_index_key_variants`) so a future `SECANTUS_ENGINE=rust` run of
    //! `test_indexes.py` sees identical bytes. No WiredTiger needed.
    use super::*;
    use bson::doc;

    /// Ascending sort-key bytes for a value (what an ASC single-field entry's
    /// `kb` is).
    fn ev(b: &Bson) -> Vec<u8> {
        sortkey::encode_value(b, None).unwrap()
    }

    #[test]
    fn escape_kb_doubles_zero_bytes() {
        assert_eq!(escape_kb(b"\x01\x00\x02"), vec![0x01, 0x00, 0xff, 0x02]);
        assert_eq!(escape_kb(b"abc"), b"abc".to_vec());
        assert_eq!(escape_kb(b"\x00\x00"), vec![0x00, 0xff, 0x00, 0xff]);
    }

    #[test]
    fn pack_entry_layout_and_unpack_roundtrip() {
        let (kb, id) = (b"\x01\x00\x02".as_slice(), b"ID".as_slice());
        let packed = pack_entry(kb, id);
        assert_eq!(packed, vec![0x01, 0x00, 0xff, 0x02, 0x00, 0x00, b'I', b'D']);
        let (esc_kb, idk) = unpack_entry(&packed);
        assert_eq!(esc_kb, escape_kb(kb).as_slice());
        assert_eq!(idk, id);
    }

    #[test]
    fn unpack_splits_on_first_separator() {
        // The id_key half may itself contain `\x00\x00`; the split must land at
        // the FIRST separator — correct because the escaped kb half can't
        // contain a bare `\x00\x00`.
        let (kb, id) = (b"\x00".as_slice(), b"\x00\x00tail".as_slice());
        let packed = pack_entry(kb, id);
        let (esc_kb, idk) = unpack_entry(&packed);
        assert_eq!(esc_kb, vec![0x00, 0xff].as_slice());
        assert_eq!(idk, id);
    }

    #[test]
    fn compound_join_inserts_separator() {
        assert_eq!(compound_join(&[vec![1, 2], vec![3]]), vec![1, 2, 0, 0, 3]);
        assert_eq!(compound_join(&[vec![9]]), vec![9]);
    }

    #[test]
    fn direction_of_numeric_only() {
        assert_eq!(direction_of(&Bson::Int32(1)), Some(1));
        assert_eq!(direction_of(&Bson::Int32(-1)), Some(-1));
        assert_eq!(direction_of(&Bson::Int64(-1)), Some(-1));
        assert_eq!(direction_of(&Bson::Double(1.0)), Some(1));
        assert_eq!(direction_of(&Bson::String("2dsphere".into())), None);
    }

    #[test]
    fn doc_makes_multikey_detects_arrays() {
        let ks = doc! {"tags": 1};
        assert!(doc_makes_multikey(&doc! {"tags": ["a", "b"]}, &ks));
        assert!(!doc_makes_multikey(&doc! {"tags": "a"}, &ks));
        assert!(!doc_makes_multikey(&doc! {"other": 1}, &ks));
    }

    #[test]
    fn variants_single_scalar_ascending() {
        let v = index_key_variants(&doc! {"_id": 1, "a": 5i32}, &doc! {"a": 1}).unwrap();
        assert_eq!(v, vec![ev(&Bson::Int32(5))]);
    }

    #[test]
    fn variants_single_descending_inverts() {
        let v = index_key_variants(&doc! {"a": 5i32}, &doc! {"a": -1}).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], sortkey::invert_bytes(&ev(&Bson::Int32(5))));
        assert_ne!(v[0], ev(&Bson::Int32(5)));
    }

    #[test]
    fn variants_missing_field_is_null() {
        let v = index_key_variants(&doc! {"_id": 1}, &doc! {"a": 1}).unwrap();
        assert_eq!(v, vec![ev(&Bson::Null)]);
    }

    #[test]
    fn variants_array_multikey_per_element_plus_whole() {
        let d = doc! {"tags": ["py", "go", "py"]};
        let v = index_key_variants(&d, &doc! {"tags": 1}).unwrap();
        // "py" deduped: element keys py, go, plus the whole-array key = 3.
        assert_eq!(v.len(), 3);
        assert!(v.contains(&ev(&Bson::String("py".into()))));
        assert!(v.contains(&ev(&Bson::String("go".into()))));
        let whole = ev(&Bson::Array(vec![
            Bson::String("py".into()),
            Bson::String("go".into()),
            Bson::String("py".into()),
        ]));
        assert!(v.contains(&whole));
    }

    #[test]
    fn variants_compound_joins_parts() {
        let v = index_key_variants(&doc! {"a": 1i32, "b": 2i32}, &doc! {"a": 1, "b": 1}).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0],
            compound_join(&[ev(&Bson::Int32(1)), ev(&Bson::Int32(2))])
        );
    }

    #[test]
    fn variants_compound_array_cartesian_product() {
        // a = [1, 2] (array), b = 9: products (1,9), (2,9), plus the whole-array
        // (([1,2]),9) combo = 3 distinct compound keys.
        let d = doc! {"a": [1i32, 2i32], "b": 9i32};
        let v = index_key_variants(&d, &doc! {"a": 1, "b": 1}).unwrap();
        assert_eq!(v.len(), 3);
        assert!(v.contains(&compound_join(&[ev(&Bson::Int32(1)), ev(&Bson::Int32(9))])));
        assert!(v.contains(&compound_join(&[ev(&Bson::Int32(2)), ev(&Bson::Int32(9))])));
    }

    #[test]
    fn id_point_lookup_classification() {
        // Bare scalar, $eq, and $in (sorted + deduped) are point lookups.
        assert_eq!(
            id_point_lookup_keys(&Bson::Int32(5)).unwrap(),
            Some(vec![ev(&Bson::Int32(5))])
        );
        assert_eq!(
            id_point_lookup_keys(&Bson::Document(doc! {"$eq": 5i32})).unwrap(),
            Some(vec![ev(&Bson::Int32(5))])
        );
        assert_eq!(
            id_point_lookup_keys(&Bson::Document(doc! {"$in": [3i32, 1i32, 3i32]}))
                .unwrap()
                .unwrap(),
            vec![ev(&Bson::Int32(1)), ev(&Bson::Int32(3))]
        );
        // Range op, literal subdocument, and operator-valued $eq are NOT.
        assert_eq!(
            id_point_lookup_keys(&Bson::Document(doc! {"$gt": 1i32})).unwrap(),
            None
        );
        assert_eq!(
            id_point_lookup_keys(&Bson::Document(doc! {"x": 1i32})).unwrap(),
            None
        );
        assert_eq!(
            id_point_lookup_keys(&Bson::Document(doc! {"$eq": {"$gt": 1i32}})).unwrap(),
            None
        );
    }
}

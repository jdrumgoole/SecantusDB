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
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document};
use secantus_core::get_path;
use secantus_core::query::matches as query_matches;
use secantus_core::sortkey::{self, COMPOUND_SEP};
use secantus_wt::{Connection, Cursor, Session, WtError};

pub mod changestreams;

const COLL_TABLE: &str = "table:secantus_collections";
const DOC_TABLE: &str = "table:secantus_documents";
const IDX_TABLE: &str = "table:secantus_indexes";
const IDX_ENTRIES_TABLE: &str = "table:secantus_index_entries";

// Oplog / change-stream tables (Phase 4 sub-phase 3). `q`-keyed (int64 seq) for
// the oplog + pre-images; a single `S` key ("state") for the recovery metadata.
const OPLOG_TABLE: &str = "table:secantus_oplog";
const PREIMAGE_TABLE: &str = "table:secantus_preimages";
const OPLOG_META_TABLE: &str = "table:secantus_oplog_meta";

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

/// A stored index, with the options the write / lookup paths care about,
/// parsed out of its registry `options` blob.
struct IndexDesc {
    name: String,
    key_spec: Document,
    sparse: bool,
    unique: bool,
    /// `partialFilterExpression` if non-empty — entries are written (and the
    /// index considered) only for docs/queries that match / imply it.
    partial: Option<Document>,
    /// `Some` for a `2d` geohash index — its field + bucketing params. Geo
    /// indexes use a separate point-only entry scheme, not `index_key_variants`.
    geo_2d: Option<Geo2d>,
}

/// A `2d` geohash index's parameters (field + bucketing range / precision).
#[derive(Clone)]
struct Geo2d {
    field: String,
    bits: u32,
    lo: f64,
    hi: f64,
}

impl Geo2d {
    /// The geohash cell (8-byte big-endian key bytes) for a point-like value, or
    /// `None` if the field value isn't a point (a `2d` index is point-only).
    fn cell_kb(&self, value: &Bson) -> Option<Vec<u8>> {
        let (x, y) = secantus_core::geo::doc_point(value)?;
        let cell = secantus_core::geo::cell_2d(x, y, self.bits, self.lo, self.hi);
        Some(secantus_core::geo::encode_cell(cell).to_vec())
    }
}

/// Parse a `2d` geo index from its key spec (`{field: "2d"}`) + options
/// (`bits` / `min` / `max`, defaulting to mongod's 26 / -180 / 180). `None` if
/// it isn't a single-field `2d` index.
fn parse_geo_2d(key_spec: &Document, opts: &Document) -> Option<Geo2d> {
    if key_spec.len() != 1 {
        return None;
    }
    let (field, v) = key_spec.iter().next().unwrap();
    if v.as_str() != Some("2d") {
        return None;
    }
    let numf = |k: &str, default: f64| -> f64 {
        match opts.get(k) {
            Some(Bson::Double(x)) => *x,
            Some(Bson::Int32(x)) => f64::from(*x),
            Some(Bson::Int64(x)) => *x as f64,
            _ => default,
        }
    };
    let bits = match opts.get("bits") {
        Some(Bson::Int32(b)) => (*b).clamp(1, 32) as u32,
        Some(Bson::Int64(b)) => (*b).clamp(1, 32) as u32,
        _ => 26,
    };
    Some(Geo2d {
        field: field.clone(),
        bits,
        lo: numf("min", -180.0),
        hi: numf("max", 180.0),
    })
}

/// A unique-index violation: the offending index plus the mongod-shaped
/// `keyPattern` / `keyValue` for the error response the command layer builds.
#[derive(Debug, Clone, PartialEq)]
pub struct UniqueConflict {
    pub index: String,
    pub key_pattern: Document,
    pub key_value: Document,
}

/// A query hint: either an index name (or `"$natural"` / `"_id_"`) or a key-spec
/// document (`{a: 1, b: -1}` / `{$natural: 1}` / `{_id: 1}`).
#[derive(Debug, Clone)]
pub enum Hint {
    Name(String),
    KeySpec(Document),
}

/// A resolved hint target.
enum ResolvedHint {
    /// `$natural` — force a collection scan.
    Natural,
    /// The virtual `_id_` index (doc-table order).
    IdIndex,
    /// A stored index by name.
    Named(String),
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
    /// A write violated a unique index. Carries the mongod-shaped conflict so the
    /// command layer can build the `E11000` error response. Boxed to keep
    /// `StorageError` (and thus `Result`) small.
    DuplicateKey(Box<UniqueConflict>),
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
    /// A `hint` did not resolve to an existing index (command layer maps this to
    /// a mongod `BadValue`).
    BadHint(String),
    /// A `fullDocument` / `fullDocumentBeforeChange: "required"` change-stream
    /// lookup missed (mongod code 280, `ChangeStreamFatalError`).
    ChangeStreamFatal(String),
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
            StorageError::DuplicateKey(c) => {
                write!(f, "E11000 duplicate key error on index {}", c.index)
            }
            StorageError::CreateIndexUnsupported(m) => write!(f, "{m}"),
            StorageError::IndexOptionsConflict(m) => write!(f, "{m}"),
            StorageError::QueryUnsupported => {
                write!(f, "query construct not supported by the Rust query engine")
            }
            StorageError::BadHint(m) => write!(f, "{m}"),
            StorageError::ChangeStreamFatal(m) => write!(f, "{m}"),
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
/// across each field's candidate values. A `sparse` index produces no keys when
/// any indexed field is missing. Missing fields otherwise encode as `null`.
/// Mirrors `storage._index_key_variants`.
fn index_key_variants(doc: &Document, key_spec: &Document, sparse: bool) -> Result<Vec<Vec<u8>>> {
    let fields: Vec<(&String, i32)> = key_spec
        .iter()
        .map(|(k, v)| (k, direction_of(v).unwrap_or(1)))
        .collect();

    if sparse && fields.iter().any(|(f, _)| get_path(doc, f).is_none()) {
        return Ok(Vec::new());
    }

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

/// The single canonical byte-key for `doc` under `key_spec` — one per doc
/// regardless of array shape (array fields encode the whole array). Used by the
/// uniqueness probe. `None` for a `sparse` index when any indexed field is
/// missing. Mirrors `storage._index_key`.
fn index_key(doc: &Document, key_spec: &Document, sparse: bool) -> Result<Option<Vec<u8>>> {
    if sparse && key_spec.keys().any(|f| get_path(doc, f).is_none()) {
        return Ok(None);
    }
    let fields: Vec<(&String, i32)> = key_spec
        .iter()
        .map(|(k, v)| (k, direction_of(v).unwrap_or(1)))
        .collect();
    if fields.len() == 1 {
        let v = get_path(doc, fields[0].0).cloned().unwrap_or(Bson::Null);
        return Ok(Some(enc_dir(&v, fields[0].1)?));
    }
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(fields.len());
    for (f, d) in &fields {
        let v = get_path(doc, f).cloned().unwrap_or(Bson::Null);
        parts.push(enc_dir(&v, *d)?);
    }
    Ok(Some(compound_join(&parts)))
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

/// True if `query` is at least as restrictive as `partial` — every key/value
/// pair in `partial` appears with the same *bare* value in `query`.
/// Conservative: operator-form clauses or document-level operators in the query
/// don't count as implying the partial filter. Mirrors
/// `storage._query_implies_partial`.
fn query_implies_partial(query: &Document, partial: &Document) -> bool {
    partial.iter().all(|(k, v)| query.get(k) == Some(v))
}

/// `(field, direction)` if `sort` is a single `±1` field (not operator-prefixed),
/// else `(None, 0)`. Mirrors `storage._single_sort_spec`.
fn single_sort_spec(sort: Option<&Document>) -> (Option<&str>, i32) {
    let s = match sort {
        Some(s) if s.len() == 1 => s,
        _ => return (None, 0),
    };
    let (f, d) = s.iter().next().unwrap();
    if f.starts_with('$') {
        return (None, 0);
    }
    match direction_of(d) {
        Some(di @ (1 | -1)) => (Some(f.as_str()), di),
        _ => (None, 0),
    }
}

/// `(field, direction)` pairs for a multi-field sort, or `None` if any entry is
/// operator-prefixed or not `±1`. Also used to validate a single-field sort for
/// the post-sort. Mirrors `storage._multi_sort_spec`.
fn multi_sort_spec(sort: Option<&Document>) -> Option<Vec<(String, i32)>> {
    let s = sort?;
    if s.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(s.len());
    for (f, d) in s {
        if f.starts_with('$') {
            return None;
        }
        match direction_of(d) {
            Some(di @ (1 | -1)) => out.push((f.clone(), di)),
            _ => return None,
        }
    }
    Some(out)
}

/// The byte-sortable compound key for `doc` under a sort `spec` — the same
/// encoding the index walk produces, so the COLLSCAN post-sort yields mongod's
/// cross-type order consistent with the accelerated path.
fn sort_key(doc: &Document, spec: &[(String, i32)]) -> Result<Vec<u8>> {
    let mut parts = Vec::with_capacity(spec.len());
    for (f, d) in spec {
        let v = get_path(doc, f).cloned().unwrap_or(Bson::Null);
        parts.push(enc_dir(&v, *d)?);
    }
    Ok(compound_join(&parts))
}

/// Build an `IxScan` plan, setting `direction` to `"backward"` when the sort
/// field is in the key spec with the opposite direction. Mirrors
/// `storage._make_ixscan_plan`.
fn make_ixscan_plan(
    name: String,
    key_spec: &Document,
    sort_field: Option<&str>,
    sort_dir: i32,
) -> ExplainPlan {
    let mut direction = "forward";
    if let Some(sf) = sort_field {
        if let Some(idx_dir) = key_spec.get(sf).and_then(direction_of) {
            if sort_dir != 0 && sort_dir != idx_dir {
                direction = "backward";
            }
        }
    }
    ExplainPlan::IxScan {
        index_name: name,
        key_pattern: key_spec.clone(),
        direction: direction.to_string(),
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
    /// Whether writes emit oplog entries (and the oplog tables are live). Mirrors
    /// `storage.enable_oplog`. Default `true`.
    enable_oplog: bool,
    /// Oplog recovery counters (next seq + last minted timestamp), guarded by a
    /// tiny dedicated mutex — `storage._oplog_seq_lock`. Held only for the
    /// microsecond seq/ts reservation, never across the WT cursor writes.
    oplog: Mutex<OplogState>,
    /// Retention window (seconds) and hard entry cap for `prune_oplog`. Mirrors
    /// `storage.oplog_retention_seconds` / `oplog_max_entries`.
    oplog_retention_seconds: i64,
    oplog_max_entries: usize,
}

/// Strictly-monotonic oplog bookkeeping: the next int64 seq to mint and the last
/// `Timestamp(secs, ord)` handed out. Recovered on open so post-restart mints
/// are strictly greater than anything previously emitted.
struct OplogState {
    next_seq: i64,
    last_ts_secs: i64,
    last_ts_ord: i64,
}

/// Milliseconds since the Unix epoch (UTC), for oplog `wall` times.
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Whole seconds since the Unix epoch (UTC), for `Timestamp.time`.
fn now_secs() -> i64 {
    now_millis() / 1000
}

/// Recover the oplog counters on open: prefer the persisted meta row, else scan
/// the oplog table for the max seq and its timestamp. Mirrors
/// `storage._load_oplog_meta`.
fn load_oplog_meta(session: &Session) -> Result<OplogState> {
    let c = session.open_cursor(OPLOG_META_TABLE, None)?;
    c.set_key_s("state");
    if c.search().is_ok() {
        let blob = c.get_value_u()?;
        if !blob.is_empty() {
            if let Ok(st) = decode_doc(&blob) {
                let g = |k: &str| {
                    st.get_i64(k)
                        .ok()
                        .or_else(|| st.get_i32(k).ok().map(i64::from))
                };
                return Ok(OplogState {
                    next_seq: g("next_seq").unwrap_or(1),
                    last_ts_secs: g("last_ts_secs").unwrap_or(0),
                    last_ts_ord: g("last_ts_ord").unwrap_or(0),
                });
            }
        }
    }
    // Fallback: reconstruct from the highest oplog row.
    let oc = session.open_cursor(OPLOG_TABLE, None)?;
    let mut last_seq = 0i64;
    let mut last_secs = 0i64;
    let mut last_ord = 0i64;
    let mut more = oc.next()?;
    while more {
        let seq = oc.get_key_q()?;
        if seq > last_seq {
            last_seq = seq;
            let blob = oc.get_value_u()?;
            if !blob.is_empty() {
                if let Ok(entry) = decode_doc(&blob) {
                    if let Some(Bson::Timestamp(ts)) = entry.get("ts") {
                        last_secs = i64::from(ts.time);
                        last_ord = i64::from(ts.increment);
                    }
                }
            }
        }
        more = oc.next()?;
    }
    Ok(OplogState {
        next_seq: last_seq + 1,
        last_ts_secs: last_secs,
        last_ts_ord: last_ord,
    })
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
        let state = {
            let boot = conn.open_session()?;
            for (name, fmt) in BOOTSTRAP {
                boot.create(name, fmt)?;
            }
            // Recover the oplog seq / timestamp counters from the meta row, or
            // reconstruct them by scanning the oplog table.
            load_oplog_meta(&boot)?
        };
        Ok(Storage {
            conn,
            lock: Mutex::new(()),
            enable_oplog: true,
            oplog: Mutex::new(state),
            oplog_retention_seconds: 3600,
            oplog_max_entries: 100_000,
        })
    }

    /// Turn oplog emission on/off (mirrors `SecantusDBServer(enable_oplog=...)`).
    /// Off means writes skip the oplog tables entirely.
    pub fn set_enable_oplog(&mut self, on: bool) {
        self.enable_oplog = on;
    }

    /// Set the oplog retention window in seconds (default 3600). Mirrors
    /// `oplog_retention_seconds`.
    pub fn set_oplog_retention_seconds(&mut self, secs: i64) {
        self.oplog_retention_seconds = secs;
    }

    /// Set the oplog hard entry cap (default 100_000). Mirrors
    /// `oplog_max_entries`.
    pub fn set_oplog_max_entries(&mut self, n: usize) {
        self.oplog_max_entries = n;
    }

    // --- oplog (Phase 4 sub-phase 3) ---

    /// Mint a strictly-monotonic `Timestamp(secs, ord)`. `ord` increments within
    /// a wall-clock second and resets to 1 on a new second. Caller holds the
    /// oplog mutex (`state`). Mirrors `storage._mint_ts`.
    fn mint_ts(state: &mut OplogState) -> bson::Timestamp {
        let now = now_secs();
        if now > state.last_ts_secs {
            state.last_ts_secs = now;
            state.last_ts_ord = 1;
        } else {
            state.last_ts_ord += 1;
        }
        bson::Timestamp {
            time: state.last_ts_secs as u32,
            increment: state.last_ts_ord as u32,
        }
    }

    /// Atomically reserve `n` consecutive seqs and mint `n` monotonic timestamps.
    /// Mirrors `storage._mint_oplog_seq_and_ts`.
    fn mint_seq_and_ts(&self, n: usize) -> (i64, Vec<bson::Timestamp>) {
        let mut st = self.oplog.lock().unwrap();
        let start = st.next_seq;
        st.next_seq += n as i64;
        let ts: Vec<bson::Timestamp> = (0..n).map(|_| Self::mint_ts(&mut st)).collect();
        (start, ts)
    }

    /// Append `entries` to the oplog table, stamping each with its minted `ts`
    /// and a `wall` time, and return the highest seq written (0 if disabled or
    /// empty). `pre_images` is parallel to `entries`; a `Some(bytes)` element is
    /// stored under the matching seq in the pre-image table. Caller holds
    /// `self.lock`. Mirrors `storage._emit_oplog` (the change-stream condvar lands
    /// in 3d).
    fn emit_oplog(
        &self,
        session: &Session,
        entries: Vec<Document>,
        pre_images: Vec<Option<Vec<u8>>>,
    ) -> Result<i64> {
        if !self.enable_oplog || entries.is_empty() {
            return Ok(0);
        }
        debug_assert_eq!(pre_images.len(), entries.len());
        let (start, ts) = self.mint_seq_and_ts(entries.len());
        let cur = session.open_cursor(OPLOG_TABLE, None)?;
        let mut pre_cur: Option<Cursor> = None;
        let wall = Bson::DateTime(bson::DateTime::from_millis(now_millis()));
        let mut last = 0i64;
        for (i, mut entry) in entries.into_iter().enumerate() {
            let seq = start + i as i64;
            entry.insert("ts", Bson::Timestamp(ts[i]));
            entry.insert("wall", wall.clone());
            let blob = encode_doc(&entry)?;
            cur.reset()?;
            cur.set_key_q(seq);
            cur.set_value_u(&blob);
            cur.insert()?;
            if let Some(pre) = &pre_images[i] {
                if pre_cur.is_none() {
                    pre_cur = Some(session.open_cursor(PREIMAGE_TABLE, None)?);
                }
                let pc = pre_cur.as_ref().unwrap();
                pc.reset()?;
                pc.set_key_q(seq);
                pc.set_value_u(pre);
                pc.insert()?;
            }
            last = seq;
        }
        Ok(last)
    }

    /// A strictly-monotonic `Timestamp` advancing the cluster clock (used for
    /// `hello`'s `lastWrite` / the `aggregate` reply's `operationTime`). Persists
    /// the recovered meta so the counter survives a restart. Mirrors
    /// `storage.current_cluster_time`.
    pub fn current_cluster_time(&self) -> Result<bson::Timestamp> {
        let _g = self.lock.lock().unwrap();
        let ts = {
            let mut st = self.oplog.lock().unwrap();
            Self::mint_ts(&mut st)
        };
        let session = self.conn.open_session()?;
        self.persist_oplog_meta(&session)?;
        Ok(ts)
    }

    /// Persist the recovery meta row (`next_seq` / `last_ts_*`). Best-effort
    /// optimisation — `load_oplog_meta` reconstructs from the oplog table if the
    /// row is stale or missing. Mirrors `storage._persist_oplog_meta`.
    fn persist_oplog_meta(&self, session: &Session) -> Result<()> {
        let mut d = Document::new();
        {
            let st = self.oplog.lock().unwrap();
            d.insert("next_seq", st.next_seq);
            d.insert("last_ts_secs", st.last_ts_secs);
            d.insert("last_ts_ord", st.last_ts_ord);
        }
        let blob = encode_doc(&d)?;
        let cur = session.open_cursor(OPLOG_META_TABLE, None)?;
        cur.set_key_s("state");
        cur.set_value_u(&blob);
        cur.insert()?; // overwrite cursor (default) -> upsert
        Ok(())
    }

    /// Forward-scan the oplog from `start_seq` (inclusive), up to `limit` entries,
    /// as `(seq, bson_bytes)` pairs. Each public call opens a fresh session, so
    /// the read view always reflects rows committed by other threads' writers.
    /// Mirrors `storage.read_oplog` (ns filtering / projection are a higher
    /// layer's job).
    pub fn read_oplog(&self, start_seq: i64, limit: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(OPLOG_TABLE, None)?;
        cur.set_key_q(start_seq);
        let mut out: Vec<(i64, Vec<u8>)> = Vec::new();
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
            let seq = cur.get_key_q()?;
            let blob = cur.get_value_u()?;
            if !blob.is_empty() {
                out.push((seq, blob));
            }
            if out.len() >= limit {
                break;
            }
            more = cur.next()?;
        }
        Ok(out)
    }

    /// The smallest seq currently present (0 if empty) — the retention floor a
    /// resume token must stay at or above. Mirrors `storage.oplog_floor_seq`.
    pub fn oplog_floor_seq(&self) -> Result<i64> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(OPLOG_TABLE, None)?;
        match cur.next() {
            Ok(true) => Ok(cur.get_key_q()?),
            Ok(false) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// The highest seq emitted (`next_seq - 1`), 0 if none. Mirrors
    /// `storage.oplog_tail_seq`.
    pub fn oplog_tail_seq(&self) -> i64 {
        let _g = self.lock.lock().unwrap();
        self.oplog.lock().unwrap().next_seq - 1
    }

    /// Merge `opts` into the collection's options blob (creating the collection
    /// if needed) — e.g. `{changeStreamPreAndPostImages: {enabled: true}}`.
    /// Mirrors `storage.set_collection_options`.
    pub fn set_collection_options(&self, db: &str, coll: &str, opts: &Document) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;
        let mut current = coll_options(&session, db, coll)?.unwrap_or_default();
        for (k, v) in opts {
            current.insert(k.clone(), v.clone());
        }
        write_coll_options(&session, db, coll, &current)
    }

    /// The collection's 16-byte UUID (minting + persisting one on first use).
    /// Mirrors `storage.collection_uuid`.
    pub fn collection_uuid(&self, db: &str, coll: &str) -> Result<Vec<u8>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;
        collection_uuid(&session, db, coll)
    }

    /// The pre-image document bytes stored for oplog `seq`, or `None`. Fresh
    /// session for cross-thread visibility. Mirrors `storage.read_preimage`.
    pub fn read_preimage(&self, seq: i64) -> Result<Option<Vec<u8>>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(PREIMAGE_TABLE, None)?;
        cur.set_key_q(seq);
        match cur.search() {
            Ok(()) => {
                let b = cur.get_value_u()?;
                Ok(if b.is_empty() { None } else { Some(b) })
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Drop oplog rows older than the retention window (`ts.time < now -
    /// retention`) and, if more than `oplog_max_entries` remain, the oldest
    /// surplus; paired pre-images go too. `now` is injected seconds (defaults to
    /// the wall clock). Returns the number of rows pruned. No background sweeper —
    /// the caller drives it. Mirrors `storage.prune_oplog` / `_prune_oplog_locked`.
    pub fn prune_oplog(&self, now: Option<i64>) -> Result<usize> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let when = now.unwrap_or_else(now_secs);
        let cutoff = when - self.oplog_retention_seconds;

        // Phase 1: collect every seq + the ones past the retention window.
        let oc = session.open_cursor(OPLOG_TABLE, None)?;
        let mut all_seqs: Vec<i64> = Vec::new();
        let mut doomed: Vec<i64> = Vec::new();
        let mut more = oc.next()?;
        while more {
            let seq = oc.get_key_q()?;
            all_seqs.push(seq);
            let blob = oc.get_value_u()?;
            if !blob.is_empty() {
                if let Ok(entry) = decode_doc(&blob) {
                    if let Some(Bson::Timestamp(ts)) = entry.get("ts") {
                        if i64::from(ts.time) < cutoff {
                            doomed.push(seq);
                        }
                    }
                }
            }
            more = oc.next()?;
        }

        // Phase 2: extend the doom set to the oldest entries over the cap.
        let kept = all_seqs.len() - doomed.len();
        if kept > self.oplog_max_entries {
            let mut extra = kept - self.oplog_max_entries;
            let doomed_set: HashSet<i64> = doomed.iter().copied().collect();
            for &seq in &all_seqs {
                if extra == 0 {
                    break;
                }
                if !doomed_set.contains(&seq) {
                    doomed.push(seq);
                    extra -= 1;
                }
            }
        }
        if doomed.is_empty() {
            return Ok(0);
        }

        let op_del = session.open_cursor(OPLOG_TABLE, None)?;
        let pre_del = session.open_cursor(PREIMAGE_TABLE, None)?;
        for &seq in &doomed {
            op_del.reset()?;
            op_del.set_key_q(seq);
            match op_del.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
            pre_del.reset()?;
            pre_del.set_key_q(seq);
            match pre_del.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(doomed.len())
    }

    /// Append one `{op: "n", ns: "", o: {msg: "periodic noop"}}` heartbeat and
    /// return its seq — keeps a quiet collection's resume token advancing with
    /// cluster time. Mirrors `storage.emit_noop_heartbeat`.
    pub fn emit_noop_heartbeat(&self) -> Result<i64> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let mut o = Document::new();
        o.insert("msg", "periodic noop");
        let mut entry = Document::new();
        entry.insert("op", "n");
        entry.insert("ns", "");
        entry.insert("o", Bson::Document(o));
        self.emit_oplog(&session, vec![entry], vec![None])
    }

    /// The smallest seq whose entry `ts >= target` (tail + 1 if none qualify) —
    /// used to resolve `startAtOperationTime`. Mirrors `storage.find_seq_for_ts`.
    pub fn find_seq_for_ts(&self, ts: bson::Timestamp) -> Result<i64> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let c = session.open_cursor(OPLOG_TABLE, None)?;
        let mut more = c.next()?;
        while more {
            let seq = c.get_key_q()?;
            let blob = c.get_value_u()?;
            if !blob.is_empty() {
                if let Ok(entry) = decode_doc(&blob) {
                    if let Some(Bson::Timestamp(e)) = entry.get("ts") {
                        if e.time > ts.time || (e.time == ts.time && e.increment >= ts.increment) {
                            return Ok(seq);
                        }
                    }
                }
            }
            more = c.next()?;
        }
        Ok(self.oplog.lock().unwrap().next_seq)
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
        // Reject unique-index violations before writing anything.
        let descs = self.index_descs(&session, db, coll)?;
        if let Some(c) = self.unique_conflict(&session, db, coll, &doc, &descs, None)? {
            return Err(StorageError::DuplicateKey(Box::new(c)));
        }
        // overwrite=false -> a pre-existing _id is a WT_DUPLICATE_KEY.
        let cur = session.open_cursor(DOC_TABLE, Some("overwrite=false"))?;
        cur.set_key_ssu(db, coll, &key);
        cur.set_value_u(&blob);
        match cur.insert() {
            Ok(()) => {}
            Err(e) if e.is_duplicate_key() => return Err(StorageError::DuplicateId),
            Err(e) => return Err(e.into()),
        }
        // Maintain secondary indexes: write this doc's entries, and lazily flag
        // any index this doc makes multikey (array value on an indexed field).
        self.write_index_entries(&session, db, coll, &doc, &descs)?;
        self.maybe_mark_multikey(&session, db, coll, &doc, &descs)?;
        // Oplog: an insert is op "i". No pre-image (there's no prior document).
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let mut o2 = Document::new();
            o2.insert("_id", id.clone());
            let mut entry = Document::new();
            entry.insert("op", "i");
            entry.insert("ns", format!("{db}.{coll}"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(doc.clone()));
            entry.insert("o2", Bson::Document(o2));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
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

        // Reject unique-index violations before mutating anything (the doc's own
        // existing entries are excluded by its id_key).
        let descs = self.index_descs(&session, db, coll)?;
        if let Some(c) = self.unique_conflict(&session, db, coll, &doc, &descs, Some(&key))? {
            return Err(StorageError::DuplicateKey(Box::new(c)));
        }

        let cur = session.open_cursor(DOC_TABLE, None)?;
        cur.set_key_ssu(db, coll, &key);
        cur.set_value_u(&blob);
        cur.update()?;

        // Maintain secondary indexes: retract the old doc's entries, write the
        // new, and lazily flag any index the new doc makes multikey (sticky —
        // the old doc's array-ness is never cleared).
        if !descs.is_empty() {
            self.delete_index_entries(&session, db, coll, &old_doc, &descs)?;
            self.write_index_entries(&session, db, coll, &doc, &descs)?;
            self.maybe_mark_multikey(&session, db, coll, &doc, &descs)?;
        }
        // Oplog: a full-document replacement is op "u" with `o` = the new doc
        // (the `$v:2` diff form is for operator-updates, which the storage layer
        // doesn't expose). The pre-image (old doc) is stored when the collection
        // has changeStreamPreAndPostImages enabled.
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let pre = if pre_post_images_enabled(&session, db, coll)? {
                Some(encode_doc(&old_doc)?)
            } else {
                None
            };
            let mut o2 = Document::new();
            o2.insert("_id", id.clone());
            let mut entry = Document::new();
            entry.insert("op", "u");
            entry.insert("ns", format!("{db}.{coll}"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(doc.clone()));
            entry.insert("o2", Bson::Document(o2));
            self.emit_oplog(&session, vec![entry], vec![pre])?;
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
        let descs = self.index_descs(&session, db, coll)?;
        self.delete_index_entries(&session, db, coll, &old_doc, &descs)?;
        // Oplog: a delete is op "d" with `o` = `o2` = {_id}. The pre-image (the
        // deleted doc) is stored when changeStreamPreAndPostImages is enabled.
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let pre = if pre_post_images_enabled(&session, db, coll)? {
                Some(encode_doc(&old_doc)?)
            } else {
                None
            };
            let mut o = Document::new();
            o.insert("_id", id.clone());
            let mut entry = Document::new();
            entry.insert("op", "d");
            entry.insert("ns", format!("{db}.{coll}"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o.clone()));
            entry.insert("o2", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![pre])?;
        }
        Ok(true)
    }

    /// Delete docs whose TTL-indexed `DateTime` field is older than `now -
    /// expireAfterSeconds`, returning the number pruned. For every index with a
    /// non-negative `expireAfterSeconds` option, the leading field is checked;
    /// docs missing the field, holding a non-date value, or inside the TTL
    /// window are left in place. The clock is injected (`now`) so tests can drive
    /// expiry — there is no background sweeper (mirrors `storage.prune_ttl`, sans
    /// the sub-phase-3 oplog emission).
    pub fn prune_ttl(&self, db: &str, coll: &str, now: bson::DateTime) -> Result<usize> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;

        // TTL indexes as (leading field, ttl seconds).
        let mut ttl: Vec<(String, f64)> = Vec::new();
        for (_name, key_spec, opts) in self.iter_indexes(&session, db, coll)? {
            let secs = match opts.get("expireAfterSeconds") {
                Some(Bson::Int32(i)) => f64::from(*i),
                Some(Bson::Int64(i)) => *i as f64,
                Some(Bson::Double(d)) => *d,
                _ => continue,
            };
            if secs < 0.0 {
                continue;
            }
            match key_spec.keys().next() {
                Some(field) => ttl.push((field.clone(), secs)),
                None => continue,
            }
        }
        if ttl.is_empty() {
            return Ok(0);
        }

        let when_ms = now.timestamp_millis();
        let descs = self.index_descs(&session, db, coll)?;
        // Snapshot candidates before mutating (no cursor walk while deleting).
        let candidates = self.scan_docs(&session, db, coll)?;
        let doc_cur = session.open_cursor(DOC_TABLE, None)?;
        let mut pruned = 0usize;
        for (id_k, blob) in candidates {
            let doc = decode_doc(&blob)?;
            let expired = ttl.iter().any(|(field, secs)| match get_path(&doc, field) {
                Some(Bson::DateTime(v)) => (when_ms - v.timestamp_millis()) as f64 / 1000.0 > *secs,
                _ => false,
            });
            if !expired {
                continue;
            }
            self.delete_index_entries(&session, db, coll, &doc, &descs)?;
            doc_cur.reset()?;
            doc_cur.set_key_ssu(db, coll, &id_k);
            match doc_cur.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
            pruned += 1;
        }
        Ok(pruned)
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
        // `2d` geo indexes are supported (point-only geohash). Other non-numeric
        // index types (2dsphere / text / hashed) are still rejected.
        let geo = parse_geo_2d(key_spec, options);
        if geo.is_none() {
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

        let mut stored_options = options.clone();
        let entries: Vec<(Vec<u8>, Vec<u8>)> = if let Some(geo) = &geo {
            // 2d geo index: one geohash cell per point-valued doc. Always flagged
            // multikey so the regular (numeric) pickers skip it.
            stored_options.insert("multikey", Bson::Boolean(true));
            let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
            for (id_k, blob) in self.scan_docs(&session, db, coll)? {
                let d = decode_doc(&blob)?;
                if let Some(kb) = get_path(&d, &geo.field).and_then(|v| geo.cell_kb(v)) {
                    out.push((kb, id_k));
                }
            }
            out
        } else {
            let sparse = options.get_bool("sparse").unwrap_or(false);
            let unique = options.get_bool("unique").unwrap_or(false);
            let partial = options
                .get_document("partialFilterExpression")
                .ok()
                .filter(|d| !d.is_empty())
                .cloned();

            // One doc-table walk: gate by the partial filter, detect multikey,
            // probe uniqueness on the canonical key, build all entry-key variants.
            let mut multikey = false;
            let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            for (id_k, blob) in self.scan_docs(&session, db, coll)? {
                let d = decode_doc(&blob)?;
                if let Some(pf) = &partial {
                    if !query_matches(&d, pf, &Document::new(), None)
                        .map_err(|_| StorageError::QueryUnsupported)?
                    {
                        continue;
                    }
                }
                if !multikey && doc_makes_multikey(&d, key_spec) {
                    multikey = true;
                }
                if unique {
                    if let Some(canonical) = index_key(&d, key_spec, sparse)? {
                        if !seen.insert(canonical) {
                            // A pre-existing doc already holds this key — can't
                            // build a unique index over the data.
                            let mut key_value = Document::new();
                            for f in key_spec.keys() {
                                key_value.insert(
                                    f.clone(),
                                    get_path(&d, f).cloned().unwrap_or(Bson::Null),
                                );
                            }
                            return Err(StorageError::DuplicateKey(Box::new(UniqueConflict {
                                index: name.to_string(),
                                key_pattern: key_spec.clone(),
                                key_value,
                            })));
                        }
                    }
                }
                for kb in index_key_variants(&d, key_spec, sparse)? {
                    entries.push((kb, id_k.clone()));
                }
            }
            if multikey {
                stored_options.insert("multikey", Bson::Boolean(true));
            }
            entries
        };
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

    /// Flag every index in `descs` that `doc` makes multikey (an array value
    /// on an indexed field) by rewriting its registry options with
    /// `multikey: true`. Sticky — never cleared. Indexes already flagged are
    /// left untouched. Mirrors `storage._maybe_mark_multikey`.
    fn maybe_mark_multikey(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        descs: &[IndexDesc],
    ) -> Result<()> {
        for desc in descs {
            let name = desc.name.as_str();
            if !doc_makes_multikey(doc, &desc.key_spec) {
                continue;
            }
            let cur = session.open_cursor(IDX_TABLE, None)?;
            cur.set_key_sss(db, coll, name);
            match cur.search() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            }
            let mut payload = decode_doc(&cur.get_value_u()?)?;
            let mut opts = payload.get_document("options").cloned().unwrap_or_default();
            if opts.get_bool("multikey").unwrap_or(false) {
                continue; // already flagged — nothing to do
            }
            opts.insert("multikey", Bson::Boolean(true));
            payload.insert("options", Bson::Document(opts));
            let blob = encode_doc(&payload)?;
            let wcur = session.open_cursor(IDX_TABLE, None)?;
            wcur.set_key_sss(db, coll, name);
            wcur.set_value_u(&blob);
            wcur.update()?;
        }
        Ok(())
    }

    /// Write `doc`'s index entries for every index in `indexes`.
    fn write_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        descs: &[IndexDesc],
    ) -> Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        let id = doc
            .get("_id")
            .ok_or_else(|| StorageError::Bson("document missing _id".into()))?;
        let id_k = id_key(id)?;
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            // 2d geo index: one cell entry per point-valued field (point-only).
            if let Some(geo) = &desc.geo_2d {
                if let Some(kb) = get_path(doc, &geo.field).and_then(|v| geo.cell_kb(v)) {
                    let packed = pack_entry(&kb, &id_k);
                    cur.reset()?;
                    cur.set_key_sssu(db, coll, &desc.name, &packed);
                    cur.set_value_u(b"");
                    cur.insert()?;
                }
                continue;
            }
            if !self.doc_in_partial(doc, desc)? {
                continue;
            }
            for kb in index_key_variants(doc, &desc.key_spec, desc.sparse)? {
                let packed = pack_entry(&kb, &id_k);
                cur.reset()?;
                cur.set_key_sssu(db, coll, &desc.name, &packed);
                cur.set_value_u(b"");
                cur.insert()?;
            }
        }
        Ok(())
    }

    /// Remove `doc`'s index entries for every index in `descs` (recomputes the
    /// same packed keys `write_index_entries` produced — same sparse / partial
    /// gating, so it removes exactly what was written).
    fn delete_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        descs: &[IndexDesc],
    ) -> Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        let id = doc
            .get("_id")
            .ok_or_else(|| StorageError::Bson("document missing _id".into()))?;
        let id_k = id_key(id)?;
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            if let Some(geo) = &desc.geo_2d {
                if let Some(kb) = get_path(doc, &geo.field).and_then(|v| geo.cell_kb(v)) {
                    let packed = pack_entry(&kb, &id_k);
                    cur.reset()?;
                    cur.set_key_sssu(db, coll, &desc.name, &packed);
                    match cur.remove() {
                        Ok(()) => {}
                        Err(e) if e.is_not_found() => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                continue;
            }
            if !self.doc_in_partial(doc, desc)? {
                continue;
            }
            for kb in index_key_variants(doc, &desc.key_spec, desc.sparse)? {
                let packed = pack_entry(&kb, &id_k);
                cur.reset()?;
                cur.set_key_sssu(db, coll, &desc.name, &packed);
                match cur.remove() {
                    Ok(()) => {}
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }

    /// Whether `doc` is covered by `desc`'s partial filter (always true for a
    /// non-partial index). A partial filter the Rust query engine can't evaluate
    /// surfaces as `QueryUnsupported`.
    fn doc_in_partial(&self, doc: &Document, desc: &IndexDesc) -> Result<bool> {
        match &desc.partial {
            None => Ok(true),
            Some(pf) => query_matches(doc, pf, &Document::new(), None)
                .map_err(|_| StorageError::QueryUnsupported),
        }
    }

    /// Parse every stored index into an `IndexDesc` (name, key_spec, sparse,
    /// unique, partial filter).
    fn index_descs(&self, session: &Session, db: &str, coll: &str) -> Result<Vec<IndexDesc>> {
        Ok(self
            .iter_indexes(session, db, coll)?
            .into_iter()
            .map(|(name, key_spec, opts)| {
                let partial = opts
                    .get_document("partialFilterExpression")
                    .ok()
                    .filter(|d| !d.is_empty())
                    .cloned();
                let geo_2d = parse_geo_2d(&key_spec, &opts);
                IndexDesc {
                    name,
                    key_spec,
                    sparse: opts.get_bool("sparse").unwrap_or(false),
                    unique: opts.get_bool("unique").unwrap_or(false),
                    partial,
                    geo_2d,
                }
            })
            .collect())
    }

    /// The first unique-index violation `candidate` would cause, or `None`.
    /// Probes the entries table for an existing row with the same canonical key
    /// belonging to a *different* doc (`exclude_id_key` skips the candidate's own
    /// row, for replace/update). Mirrors `storage._unique_conflict`.
    fn unique_conflict(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        candidate: &Document,
        descs: &[IndexDesc],
        exclude_id_key: Option<&[u8]>,
    ) -> Result<Option<UniqueConflict>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            if !desc.unique || !self.doc_in_partial(candidate, desc)? {
                continue;
            }
            let kb = match index_key(candidate, &desc.key_spec, desc.sparse)? {
                Some(k) => k,
                None => continue,
            };
            let esc_kb = escape_kb(&kb);
            let mut seed = esc_kb.clone();
            seed.extend_from_slice(ENTRY_SEP);
            cur.reset()?;
            cur.set_key_sssu(db, coll, &desc.name, &seed);
            let mut more = match cur.search_near() {
                Ok(cmp) => {
                    if cmp < 0 {
                        cur.next()?
                    } else {
                        true
                    }
                }
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e.into()),
            };
            while more {
                let (d, c, n, packed) = cur.get_key_sssu()?;
                if d != db || c != coll || n != desc.name {
                    break;
                }
                let (row_esc, row_id) = unpack_entry(&packed);
                if row_esc != esc_kb.as_slice() {
                    break;
                }
                let is_self = exclude_id_key == Some(row_id);
                if !is_self {
                    let mut key_value = Document::new();
                    for f in desc.key_spec.keys() {
                        key_value.insert(
                            f.clone(),
                            get_path(candidate, f).cloned().unwrap_or(Bson::Null),
                        );
                    }
                    return Ok(Some(UniqueConflict {
                        index: desc.name.clone(),
                        key_pattern: desc.key_spec.clone(),
                        key_value,
                    }));
                }
                more = cur.next()?;
            }
        }
        Ok(None)
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

    // --- query routing (Phase 4 sub-phase 2) ---

    /// Documents matching `filter`, as BSON bytes, in `_id`-natural / index order.
    /// Convenience wrapper for `find_matching_with(.., None, None)`.
    pub fn find_matching(&self, db: &str, coll: &str, filter: &Document) -> Result<Vec<Vec<u8>>> {
        self.find_matching_with(db, coll, filter, None, None)
    }

    /// Documents matching `filter`, as BSON bytes, ordered per `sort` and routed
    /// per `hint`. Routes the filter through an index (single-field / compound
    /// equality / `$in` / range, or the `_id` point lookup) else a collection
    /// scan; index candidates are re-checked with `matches()`. When `sort` can be
    /// satisfied by walking an index (the filter field equals the sort field, or
    /// the filter is empty and a single-field / compound index matches the sort)
    /// the results come back already ordered and the post-sort is skipped;
    /// otherwise they're sorted with the byte-sortable key encoder (mongod
    /// cross-type order). `hint` forces an index / `$natural` scan. Mirrors
    /// `storage.find_matching` (skip / limit / projection stay in the command
    /// layer).
    pub fn find_matching_with(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<&Hint>,
    ) -> Result<Vec<Vec<u8>>> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let (sort_field, sort_dir) = single_sort_spec(sort);
        let mut in_sort_order = false;

        let blobs: Vec<Vec<u8>> = if let Some(h) = hint {
            let resolved = self.resolve_hint(&session, db, coll, h)?;
            let (cands, ord) =
                self.candidates_from_hint(&session, db, coll, &resolved, sort_field, sort_dir)?;
            in_sort_order = ord;
            cands
        } else if let Some(id_keys) = self.try_index_id_keys(&session, db, coll, filter)? {
            let mut docs = self.docs_by_id_keys(&session, db, coll, &id_keys)?;
            // Single-field filter on the sort field: the index walk already
            // ordered the candidates (modulo direction).
            if let Some(sf) = sort_field {
                let single = filter.len() == 1 && filter.keys().next().is_some_and(|f| f == sf);
                if single {
                    in_sort_order = true;
                    let idx_dir = self
                        .find_leading_field_index(&session, db, coll, sf, filter)?
                        .map(|m| m.1)
                        .unwrap_or(1);
                    if sort_dir != idx_dir {
                        docs.reverse();
                    }
                }
            }
            docs
        } else if filter.is_empty() {
            if let Some(sf) = sort_field {
                // Single-field sort: walk a leading-field index, else COLLSCAN.
                match self.find_leading_field_index(&session, db, coll, sf, filter)? {
                    Some((idx_name, idx_dir, _is_compound)) => {
                        in_sort_order = true;
                        self.walk_index_in_order(
                            &session,
                            db,
                            coll,
                            &idx_name,
                            sort_dir != idx_dir,
                        )?
                    }
                    None => self.scan_blobs(&session, db, coll)?,
                }
            } else if let Some(multi) = multi_sort_spec(sort).filter(|m| m.len() > 1) {
                // Multi-field sort: walk a strict-match compound index, else COLLSCAN.
                match self.compound_index_for_sort(&session, db, coll, &multi)? {
                    Some((idx_name, reverse)) => {
                        in_sort_order = true;
                        self.walk_index_in_order(&session, db, coll, &idx_name, reverse)?
                    }
                    None => self.scan_blobs(&session, db, coll)?,
                }
            } else {
                self.scan_blobs(&session, db, coll)?
            }
        } else {
            self.scan_blobs(&session, db, coll)?
        };

        // Decode + filter; keep the doc alongside the blob for the post-sort.
        let vars = Document::new();
        let mut out: Vec<(Document, Vec<u8>)> = Vec::new();
        for blob in blobs {
            let d = decode_doc(&blob)?;
            if query_matches(&d, filter, &vars, None).map_err(|_| StorageError::QueryUnsupported)? {
                out.push((d, blob));
            }
        }
        if !in_sort_order {
            if let Some(spec) = multi_sort_spec(sort) {
                // Decorate-sort-undecorate on the byte-sortable compound key.
                let mut keyed: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(out.len());
                for (d, blob) in out {
                    keyed.push((sort_key(&d, &spec)?, blob));
                }
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                return Ok(keyed.into_iter().map(|(_, b)| b).collect());
            }
        }
        Ok(out.into_iter().map(|(_, b)| b).collect())
    }

    /// The plan `find_matching` would use for `filter` (no execution).
    /// Convenience wrapper for `explain_plan_with(.., None, None)`.
    pub fn explain_plan(&self, db: &str, coll: &str, filter: &Document) -> Result<ExplainPlan> {
        self.explain_plan_with(db, coll, filter, None, None)
    }

    /// The plan `find_matching_with` would use for these args (no execution),
    /// honouring `sort` (sets the walk `direction`) and `hint`. A `hint` that
    /// doesn't resolve to an index degrades to COLLSCAN (mirroring
    /// `storage.explain_plan`, which catches `BadHint`).
    pub fn explain_plan_with(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<&Hint>,
    ) -> Result<ExplainPlan> {
        let _g = self.lock.lock().unwrap();
        let session = self.conn.open_session()?;
        let (sort_field, sort_dir) = single_sort_spec(sort);

        if let Some(h) = hint {
            let resolved = match self.resolve_hint(&session, db, coll, h) {
                Ok(r) => r,
                Err(StorageError::BadHint(_)) => return Ok(ExplainPlan::CollScan),
                Err(e) => return Err(e),
            };
            return match resolved {
                ResolvedHint::Natural => Ok(ExplainPlan::CollScan),
                ResolvedHint::IdIndex => {
                    let direction = if sort_field == Some("_id") && sort_dir == -1 {
                        "backward"
                    } else {
                        "forward"
                    };
                    let mut kp = Document::new();
                    kp.insert("_id", 1i32);
                    Ok(ExplainPlan::IxScan {
                        index_name: ID_INDEX_NAME.to_string(),
                        key_pattern: kp,
                        direction: direction.to_string(),
                    })
                }
                ResolvedHint::Named(name) => match self.key_spec_for(&session, db, coll, &name)? {
                    Some(key_spec) => Ok(make_ixscan_plan(name, &key_spec, sort_field, sort_dir)),
                    None => Ok(ExplainPlan::CollScan),
                },
            };
        }

        if let Some((name, key_spec)) = self.pick_index_for_filter(&session, db, coll, filter)? {
            return Ok(make_ixscan_plan(name, &key_spec, sort_field, sort_dir));
        }
        if filter.is_empty() {
            if let Some(sf) = sort_field {
                if let Some((name, _dir, _comp)) =
                    self.find_leading_field_index(&session, db, coll, sf, filter)?
                {
                    if let Some(key_spec) = self.key_spec_for(&session, db, coll, &name)? {
                        return Ok(make_ixscan_plan(name, &key_spec, sort_field, sort_dir));
                    }
                }
            } else if sort.is_some() {
                if let Some(multi) = multi_sort_spec(sort).filter(|m| m.len() > 1) {
                    if let Some((name, reverse)) =
                        self.compound_index_for_sort(&session, db, coll, &multi)?
                    {
                        if let Some(key_spec) = self.key_spec_for(&session, db, coll, &name)? {
                            return Ok(ExplainPlan::IxScan {
                                index_name: name,
                                key_pattern: key_spec,
                                direction: if reverse { "backward" } else { "forward" }.to_string(),
                            });
                        }
                    }
                }
            }
        }
        Ok(ExplainPlan::CollScan)
    }

    /// Raw doc blobs for `(db, coll)` in natural `_id` order.
    fn scan_blobs(&self, session: &Session, db: &str, coll: &str) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .scan_docs(session, db, coll)?
            .into_iter()
            .map(|(_id_k, blob)| blob)
            .collect())
    }

    /// Resolve `hint` to an index name / `$natural` / `_id_`. Mirrors
    /// `storage._resolve_hint`.
    fn resolve_hint(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        hint: &Hint,
    ) -> Result<ResolvedHint> {
        match hint {
            Hint::Name(s) => {
                if s == "$natural" {
                    return Ok(ResolvedHint::Natural);
                }
                if s == ID_INDEX_NAME {
                    return Ok(ResolvedHint::IdIndex);
                }
                for (name, _k, _o) in self.iter_indexes(session, db, coll)? {
                    if &name == s {
                        return Ok(ResolvedHint::Named(name));
                    }
                }
                Err(StorageError::BadHint(format!(
                    "hint {s:?} does not correspond to an existing index"
                )))
            }
            Hint::KeySpec(spec) => {
                if spec.len() == 1 && spec.contains_key("$natural") {
                    return Ok(ResolvedHint::Natural);
                }
                if spec.len() == 1 && spec.get("_id").and_then(direction_of) == Some(1) {
                    return Ok(ResolvedHint::IdIndex);
                }
                for (name, key_spec, _o) in self.iter_indexes(session, db, coll)? {
                    if &key_spec == spec {
                        return Ok(ResolvedHint::Named(name));
                    }
                }
                Err(StorageError::BadHint(format!(
                    "hint {spec:?} does not correspond to an existing index"
                )))
            }
        }
    }

    /// Candidate doc blobs for a resolved hint, plus whether they're already in
    /// the requested sort order. Mirrors `storage._candidates_from_hint`.
    fn candidates_from_hint(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        resolved: &ResolvedHint,
        sort_field: Option<&str>,
        sort_dir: i32,
    ) -> Result<(Vec<Vec<u8>>, bool)> {
        match resolved {
            ResolvedHint::Natural => Ok((self.scan_blobs(session, db, coll)?, false)),
            ResolvedHint::IdIndex => {
                // The doc table is keyed by id_key, so a natural scan is _id order.
                let mut docs = self.scan_blobs(session, db, coll)?;
                let in_order = sort_field == Some("_id");
                if in_order && sort_dir == -1 {
                    docs.reverse();
                }
                Ok((docs, in_order))
            }
            ResolvedHint::Named(name) => {
                let mut leading: Option<(String, i32)> = None;
                for (n, key_spec, _o) in self.iter_indexes(session, db, coll)? {
                    if &n == name {
                        if let Some((f, dv)) = key_spec.iter().next() {
                            leading = Some((f.clone(), direction_of(dv).unwrap_or(1)));
                        }
                        break;
                    }
                }
                let mut docs = self.walk_index_in_order(session, db, coll, name, false)?;
                let in_order = match (&leading, sort_field) {
                    (Some((f, _)), Some(sf)) => f == sf,
                    _ => false,
                };
                if in_order && sort_dir != leading.as_ref().map(|l| l.1).unwrap_or(1) {
                    docs.reverse();
                }
                Ok((docs, in_order))
            }
        }
    }

    /// All docs of an index, in WT entry order (or reversed), deduped — for
    /// sort-by-index walks. Mirrors `storage._walk_index_in_order`.
    fn walk_index_in_order(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
        reverse: bool,
    ) -> Result<Vec<Vec<u8>>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        cur.set_key_sssu(db, coll, name, b"");
        let mut id_keys: Vec<Vec<u8>> = Vec::new();
        let mut more = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        while more {
            let (d, c, n, packed) = cur.get_key_sssu()?;
            if d != db || c != coll || n != name {
                break;
            }
            let (_esc, row_id) = unpack_entry(&packed);
            id_keys.push(row_id.to_vec());
            more = cur.next()?;
        }
        if reverse {
            id_keys.reverse();
        }
        self.docs_by_id_keys(session, db, coll, &id_keys)
    }

    /// A compound index whose key spec exactly matches `sort_fields` (forward) or
    /// fully inverts it (backward walk). Multikey indexes are excluded (array
    /// values break the natural-order walk). Strict shape only. Mirrors
    /// `storage._compound_index_for_sort`.
    fn compound_index_for_sort(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        sort_fields: &[(String, i32)],
    ) -> Result<Option<(String, bool)>> {
        let inverted: Vec<(String, i32)> =
            sort_fields.iter().map(|(f, d)| (f.clone(), -d)).collect();
        for (name, key_spec, opts) in self.iter_indexes(session, db, coll)? {
            if opts.get_bool("multikey").unwrap_or(false) {
                continue;
            }
            let idx_pairs: Vec<(String, i32)> = match key_spec
                .iter()
                .map(|(f, d)| direction_of(d).map(|di| (f.clone(), di)))
                .collect::<Option<Vec<_>>>()
            {
                Some(p) if p.iter().all(|(_, d)| *d == 1 || *d == -1) => p,
                _ => continue,
            };
            if idx_pairs == sort_fields {
                return Ok(Some((name, false)));
            }
            if idx_pairs == inverted {
                return Ok(Some((name, true)));
            }
        }
        Ok(None)
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
        // Geo dispatch: a `$geoWithin` on a 2d-indexed field scans the geohash
        // covering range.
        if let Some(r) = self.try_geo_2d_id_keys(session, db, coll, filter)? {
            return Ok(Some(r));
        }
        // Bare-equality filters of any size can use a compound (or single-field)
        // index whose leading fields cover them.
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
        let idx = match self.find_leading_field_index(session, db, coll, field, filter)? {
            Some(m) => m,
            None => return Ok(None),
        };
        self.lookup_id_keys_via_leading_field(session, db, coll, &idx, value)
    }

    /// Find the `2d` index covering `field`, as `(index_name, params)`.
    fn geo_2d_for(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        field: &str,
    ) -> Result<Option<(String, Geo2d)>> {
        for desc in self.index_descs(session, db, coll)? {
            if let Some(g) = &desc.geo_2d {
                if g.field == field {
                    return Ok(Some((desc.name.clone(), g.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Candidate `id_key`s for `{field: {$geoWithin: <region>}}` via a `2d`
    /// index: scan the Z-order geohash range covering the region's bounding box
    /// (a superset — `find_matching` re-checks each with `matches()`). `None`
    /// (→ COLLSCAN) if there's no 2d index on `field`, the filter isn't a lone
    /// `$geoWithin`, or the region's matching itself defers (e.g. `$center`).
    fn try_geo_2d_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        if filter.len() != 1 {
            return Ok(None);
        }
        let (field, value) = filter.iter().next().unwrap();
        let within = match value {
            Bson::Document(d) => match d.get_document("$geoWithin") {
                Ok(w) => w,
                Err(_) => return Ok(None),
            },
            _ => return Ok(None),
        };
        let (name, g) = match self.geo_2d_for(session, db, coll, field)? {
            Some(x) => x,
            None => return Ok(None),
        };
        let (min_x, min_y, max_x, max_y) = match secantus_core::geo::query_within_bbox(within) {
            Some(b) => b,
            None => return Ok(None),
        };
        let (clo, chi) =
            secantus_core::geo::covering_2d(min_x, min_y, max_x, max_y, g.bits, g.lo, g.hi);
        let lo_kb = secantus_core::geo::encode_cell(clo);
        let hi_kb = secantus_core::geo::encode_cell(chi);
        let ids = self.range_scan_index(
            session,
            db,
            coll,
            &name,
            Some(&lo_kb[..]),
            true,
            Some(&hi_kb[..]),
            true,
            None,
        )?;
        Ok(Some(ids))
    }

    /// The best index whose leading field is `field`, as `(name, direction,
    /// is_compound)`. Single-field indexes win over compound (tighter scan);
    /// otherwise the first compound index with that leading field is the
    /// fallback. Skips non-`1`/`-1` directions (geo / text / hashed) and partial
    /// indexes the `query` doesn't imply. (Collation gating is deferred.)
    /// Multikey indexes are NOT skipped — per-element entries cover the lookup,
    /// and `find_matching` re-checks with `matches()`. Mirrors
    /// `storage._find_leading_field_index`.
    fn find_leading_field_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        field: &str,
        query: &Document,
    ) -> Result<Option<(String, i32, bool)>> {
        let mut compound_fallback: Option<(String, i32, bool)> = None;
        for desc in self.index_descs(session, db, coll)? {
            if let Some(pf) = &desc.partial {
                if !query_implies_partial(query, pf) {
                    continue;
                }
            }
            let n_fields = desc.key_spec.len();
            let leads = desc
                .key_spec
                .keys()
                .next()
                .is_some_and(|f| f.as_str() == field);
            if !leads {
                continue;
            }
            if !desc
                .key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let d = direction_of(desc.key_spec.get(field).unwrap()).unwrap();
            if n_fields == 1 {
                return Ok(Some((desc.name, d, false)));
            }
            if compound_fallback.is_none() {
                compound_fallback = Some((desc.name, d, true));
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
        // Geo: a `$geoWithin` served by a 2d index (mirrors try_geo_2d_id_keys).
        if self
            .try_geo_2d_id_keys(session, db, coll, filter)?
            .is_some()
        {
            let (field, _) = filter.iter().next().unwrap();
            if let Some((name, _g)) = self.geo_2d_for(session, db, coll, field)? {
                let mut kp = Document::new();
                kp.insert(field.clone(), "2d");
                return Ok(Some((name, kp)));
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
        let idx = match self.find_leading_field_index(session, db, coll, field, filter)? {
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
    /// preferring the shortest. A partial index is considered only when the
    /// filter implies its partial filter, and the partial-filter keys are
    /// stripped from the effective filter fields (the index guarantees them).
    /// `None` if none covers it. Mirrors `storage._pick_compound_eq_index`.
    /// (Collation gating is deferred.)
    fn pick_compound_eq_index(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<(String, Document)>> {
        let filter_fields: HashSet<&str> = filter.keys().map(|s| s.as_str()).collect();
        let mut best: Option<(String, Document)> = None;
        for desc in self.index_descs(session, db, coll)? {
            let eff_fields: HashSet<&str> = match &desc.partial {
                Some(pf) => {
                    if !query_implies_partial(filter, pf) {
                        continue;
                    }
                    filter_fields
                        .iter()
                        .copied()
                        .filter(|f| !pf.contains_key(*f))
                        .collect()
                }
                None => filter_fields.clone(),
            };
            let eff_len = eff_fields.len();
            if !desc
                .key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let idx_fields: Vec<&String> = desc.key_spec.keys().collect();
            if idx_fields.len() < eff_len {
                continue;
            }
            let prefix_set: HashSet<&str> =
                idx_fields[..eff_len].iter().map(|s| s.as_str()).collect();
            if prefix_set != eff_fields {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(_, b)| b.len() > idx_fields.len())
            {
                best = Some((desc.name.clone(), desc.key_spec.clone()));
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
        // Index-order fields that the filter constrains. Partial-filter clauses
        // live outside the key (the picker already verified the filter implies
        // them), so an index field absent from the filter just isn't pinned.
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
    /// A partial index is considered only when the filter implies its partial
    /// filter. `None` if none fits. Mirrors `storage._pick_compound_range_index`.
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
        for desc in self.index_descs(session, db, coll)? {
            if let Some(pf) = &desc.partial {
                if !query_implies_partial(filter, pf) {
                    continue;
                }
            }
            if !desc
                .key_spec
                .values()
                .all(|v| matches!(direction_of(v), Some(1) | Some(-1)))
            {
                continue;
            }
            let idx_fields: Vec<&String> = desc.key_spec.keys().collect();
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
                best = Some((desc.name.clone(), desc.key_spec.clone()));
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

/// The collection's options document (`{}` when registered with none), or
/// `None` when the collection isn't registered.
fn coll_options(session: &Session, db: &str, coll: &str) -> Result<Option<Document>> {
    let cur = session.open_cursor(COLL_TABLE, None)?;
    cur.set_key_ss(db, coll);
    match cur.search() {
        Ok(()) => {
            let blob = cur.get_value_u()?;
            if blob.is_empty() {
                Ok(Some(Document::new()))
            } else {
                Ok(Some(decode_doc(&blob)?))
            }
        }
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Overwrite the collection's options blob (caller has ensured registration).
fn write_coll_options(session: &Session, db: &str, coll: &str, opts: &Document) -> Result<()> {
    let blob = encode_doc(opts)?;
    let cur = session.open_cursor(COLL_TABLE, None)?;
    cur.set_key_ss(db, coll);
    cur.set_value_u(&blob);
    cur.insert()?; // overwrite cursor (default) -> upsert
    Ok(())
}

/// The collection's UUID (16 bytes), minting + persisting one into the options on
/// first use. Mirrors `storage._collection_uuid`.
fn collection_uuid(session: &Session, db: &str, coll: &str) -> Result<Vec<u8>> {
    let mut opts = coll_options(session, db, coll)?.unwrap_or_default();
    if let Some(Bson::Binary(b)) = opts.get("uuid") {
        if b.bytes.len() == 16 {
            return Ok(b.bytes.clone());
        }
    }
    let bytes = new_uuid_bytes().to_vec();
    opts.insert("uuid", uuid_binary(&bytes));
    write_coll_options(session, db, coll, &opts)?;
    Ok(bytes)
}

/// Whether `changeStreamPreAndPostImages.enabled` is set on the collection.
fn pre_post_images_enabled(session: &Session, db: &str, coll: &str) -> Result<bool> {
    if let Some(opts) = coll_options(session, db, coll)? {
        if let Ok(sub) = opts.get_document("changeStreamPreAndPostImages") {
            return Ok(sub.get_bool("enabled").unwrap_or(false));
        }
    }
    Ok(false)
}

/// A fresh 16-byte UUID. No `uuid` crate dependency — two `ObjectId`s (which use
/// `getrandom` + a per-process counter) supply the entropy; the version / variant
/// nibbles are set cosmetically (the `ui` field is opaque to drivers).
fn new_uuid_bytes() -> [u8; 16] {
    let a = ObjectId::new();
    let b = ObjectId::new();
    let mut out = [0u8; 16];
    out[..12].copy_from_slice(&a.bytes());
    out[12..16].copy_from_slice(&b.bytes()[..4]);
    out[6] = (out[6] & 0x0f) | 0x40;
    out[8] = (out[8] & 0x3f) | 0x80;
    out
}

/// Wrap 16 UUID bytes as a BSON Binary subtype 4 (mongod's `ui` encoding).
fn uuid_binary(bytes: &[u8]) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Uuid,
        bytes: bytes.to_vec(),
    })
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
        let v = index_key_variants(&doc! {"_id": 1, "a": 5i32}, &doc! {"a": 1}, false).unwrap();
        assert_eq!(v, vec![ev(&Bson::Int32(5))]);
    }

    #[test]
    fn variants_single_descending_inverts() {
        let v = index_key_variants(&doc! {"a": 5i32}, &doc! {"a": -1}, false).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], sortkey::invert_bytes(&ev(&Bson::Int32(5))));
        assert_ne!(v[0], ev(&Bson::Int32(5)));
    }

    #[test]
    fn variants_missing_field_is_null() {
        let v = index_key_variants(&doc! {"_id": 1}, &doc! {"a": 1}, false).unwrap();
        assert_eq!(v, vec![ev(&Bson::Null)]);
    }

    #[test]
    fn variants_array_multikey_per_element_plus_whole() {
        let d = doc! {"tags": ["py", "go", "py"]};
        let v = index_key_variants(&d, &doc! {"tags": 1}, false).unwrap();
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
        let v = index_key_variants(&doc! {"a": 1i32, "b": 2i32}, &doc! {"a": 1, "b": 1}, false)
            .unwrap();
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
        let v = index_key_variants(&d, &doc! {"a": 1, "b": 1}, false).unwrap();
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

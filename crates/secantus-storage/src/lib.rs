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
//! * documents live at `(db, coll, RecordId) -> [id_key_len][id_key][bson(doc)]`
//!   where the RecordId is a monotonic per-insertion counter — so iterating the
//!   table yields insertion (natural / RecordId) order, and the `id_key`
//!   (`sortkey.encode_value(_id)`, needed to maintain the `_id` + secondary
//!   indexes and NOT reconstructable for suffixed timeseries keys) rides in-band
//!   in the value (see `frame_doc_value`). The `_id` index (`secantus_natural_seq`:
//!   `(db, coll, id_key) -> RecordId`) resolves an `_id` to its RecordId;
//! * inserts write the `_id` index with a non-overwriting cursor so a duplicate
//!   `_id` surfaces as a duplicate-key error;
//! * a global lock serialises public methods (1:1 with `storage.py`'s `RLock`).
//!
//! Later sub-phases add indexes, geo, and the oplog (see
//! `tasks/rust-rewrite-phase4-scoping.md`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document};
use s2::cellid::CellID;
use s2::latlng::LatLng;
use s2::rect::Rect;
use s2::region::RegionCoverer;
use secantus_core::collation::Collation;
use secantus_core::diff::compute_update_description;
use secantus_core::query::matches as query_matches;
use secantus_core::sortkey::{self, COMPOUND_SEP};
use secantus_core::{get_path, get_path_values};
use secantus_wt::{Connection, Cursor, Session, WtError};

pub mod changestreams;
pub mod pitr_archive;
pub mod replay;

/// Filename of the advisory PITR manifest embedded in a backup archive.
pub(crate) const PITR_MANIFEST_NAME: &str = "pitr-manifest.json";

use std::cell::Cell;

thread_local! {
    /// The active multi-document-transaction session for the current thread, or
    /// null when not inside an in-transaction statement. Installed by
    /// [`Storage::with_user_transaction`] for the duration of a statement so
    /// [`Storage::op_session`] routes the statement's cursors through the
    /// transaction's WT session (read-your-own-writes + the pinned snapshot fall
    /// out for free). Cleared (RAII, panic-safe) before the call returns.
    static ACTIVE_TXN_SESSION: Cell<*const Session> = const { Cell::new(std::ptr::null()) };
}

/// The WT session one storage operation runs on: either a borrowed
/// multi-document-transaction session (left open across the transaction's
/// statements) or a freshly-opened autocommit session (closed when this drops).
/// Derefs to `Session` so call sites are identical for both.
enum OpSession<'a> {
    Fresh(Session),
    Txn(&'a Session),
}

impl std::ops::Deref for OpSession<'_> {
    type Target = Session;
    fn deref(&self) -> &Session {
        match self {
            OpSession::Fresh(s) => s,
            OpSession::Txn(s) => s,
        }
    }
}

/// Opaque handle for a multi-document transaction. Owns a **dedicated** WT
/// session (NOT the calling thread's per-call session) so the transaction's
/// statements and its retryable commit can run on different connection threads
/// — `Session` is `Send`, and the command layer's per-transaction mutex
/// guarantees the session is never touched by two threads at once. The WT
/// `begin_transaction` is deferred to the first [`Storage::with_user_transaction`]
/// so the snapshot pins at the transaction's first statement (mongod semantics).
///
/// `session` is `Some` while the transaction is open and `None` once committed /
/// rolled back — committing or aborting **closes** the dedicated WT session so it
/// doesn't accumulate (the registry keeps the `Transaction` metadata around for
/// idempotent re-commit, but the WT resource is released). Mirrors
/// `storage._close_user_txn_session`.
pub struct UserTransactionHandle {
    session: Option<Session>,
    began: bool,
}

/// mongod's per-document BSON size limit (16 MiB). A document whose encoded size
/// exceeds this is rejected with `BSONObjectTooLarge` (10334). Mirrors
/// `storage.py`'s `MAX_BSON_OBJECT_SIZE`.
const MAX_BSON_OBJECT_SIZE: usize = 16 * 1024 * 1024;

/// The mongod-shaped per-op write error for an over-limit document (10334).
fn too_large_write_error(index: usize, size: usize) -> Document {
    bson::doc! {
        "index": index as i32,
        "code": 10334,
        "errmsg": format!(
            "object to insert too large. size in bytes: {size}, max size: {MAX_BSON_OBJECT_SIZE}"
        ),
    }
}

const COLL_TABLE: &str = "table:secantus_collections";
/// Legacy single documents table. Retained for the on-disk upgrade read/migration
/// (a store written by an older build has its rows here). New writes go to the
/// per-collection shard tables — see `DOC_SHARDS` / `doc_table_for`.
const DOC_TABLE: &str = "table:secantus_documents";
/// The documents table is sharded across `DOC_SHARDS` WT tables, routed by a
/// deterministic hash of `(db, coll)`. Every collection lives ENTIRELY in one
/// shard, so per-collection ops (insert / find / scan / update / delete) touch a
/// single shard with no merge — the point is that concurrent writers to different
/// collections land on different WT files, and thus different block-manager locks
/// and cache regions, instead of all serialising on one `secantus_documents` file
/// (the 8-writer scaling bottleneck; `tasks/rust-mongodb-parity-redesign.md`).
const DOC_SHARDS: u64 = 16;

/// Deterministic FNV-1a hash of `(db, coll)` — stable across process restarts (a
/// collection must always resolve to the same shard). `std`'s DefaultHasher is
/// randomised per run, so it can't be used here.
fn doc_shard_hash(db: &str, coll: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in db.bytes().chain(std::iter::once(0u8)).chain(coll.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Shard table name for shard index `s` (0..DOC_SHARDS).
fn doc_shard_name(s: u64) -> String {
    format!("table:secantus_documents_sh{s}")
}

/// The documents shard table a `(db, coll)` lives in.
fn doc_table_for(db: &str, coll: &str) -> String {
    doc_shard_name(doc_shard_hash(db, coll) % DOC_SHARDS)
}

/// A scanned document row: `(RecordId, id_key, blob)`. The RecordId is the
/// doc-table key; the id_key and blob are unframed from the value (see
/// [`frame_doc_value`]).
type ScannedDoc = (i64, Vec<u8>, Vec<u8>);

/// One entry to persist to the oplog, before `ts` / `wall` are stamped on.
/// The hot CRUD paths build [`OplogEntry::Raw`] (op / ns / ui / o / o2 already
/// serialized, the document body spliced in without re-encoding — the oplog
/// hot-path win); the rarer DDL / noop / findAndModify paths pass an owned
/// [`OplogEntry::Doc`] that is encoded the historical way.
enum OplogEntry {
    Doc(Document),
    Raw(bson::RawDocumentBuf),
}

/// Frame a doc-table value as `[u32-LE id_key_len][id_key bytes][blob bytes]`.
/// The doc table is keyed by RecordId, so the `id_key` (needed to drop the doc's
/// `_id`-index + secondary-index entries on delete, and NOT reconstructable from
/// `_id` for timeseries docs, whose key carries a non-derivable suffix) is stored
/// in-band alongside the blob. The value stays a single opaque `u` column — no WT
/// schema change; the framing lives entirely in our encode/decode.
fn frame_doc_value(id_key: &[u8], blob: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + id_key.len() + blob.len());
    v.extend_from_slice(&(id_key.len() as u32).to_le_bytes());
    v.extend_from_slice(id_key);
    v.extend_from_slice(blob);
    v
}

/// Byte offset where the blob begins in a framed doc-table value (see
/// [`frame_doc_value`]): `4 + id_key_len`. Lets a read-only scan strip the frame
/// prefix in place (reusing the value's allocation) instead of re-copying the
/// blob out.
fn frame_prefix_len(value: &[u8]) -> Result<usize> {
    if value.len() < 4 {
        return Err(StorageError::Internal(
            "doc-table value shorter than 4-byte frame header".into(),
        ));
    }
    let len = u32::from_le_bytes([value[0], value[1], value[2], value[3]]) as usize;
    let prefix = 4 + len;
    if value.len() < prefix {
        return Err(StorageError::Internal(
            "doc-table value id_key length exceeds frame".into(),
        ));
    }
    Ok(prefix)
}

/// Split a framed doc-table value (see [`frame_doc_value`]) into `(id_key, blob)`.
fn unframe_doc_value(value: &[u8]) -> Result<(&[u8], &[u8])> {
    if value.len() < 4 {
        return Err(StorageError::Internal(
            "doc-table value shorter than 4-byte frame header".into(),
        ));
    }
    let len = u32::from_le_bytes([value[0], value[1], value[2], value[3]]) as usize;
    let rest = &value[4..];
    if rest.len() < len {
        return Err(StorageError::Internal(
            "doc-table value id_key length exceeds frame".into(),
        ));
    }
    Ok((&rest[..len], &rest[len..]))
}

/// One-time migration: move every row in the legacy single `secantus_documents`
/// table to its per-collection shard (a store written before doc-sharding). A
/// born-sharded store's legacy table is empty, so this is a quick no-op scan on
/// open. Runs on the bootstrap session before the connection serves requests.
fn migrate_legacy_docs(session: &Session) -> Result<()> {
    let src = session.open_cursor(DOC_TABLE, None)?;
    let mut rows: Vec<(String, String, Vec<u8>, Vec<u8>)> = Vec::new();
    let mut more = src.next()?;
    while more {
        let (db, coll, id_key) = src.get_key_ssu()?;
        let blob = src.get_value_u()?;
        rows.push((db, coll, id_key, blob));
        more = src.next()?;
    }
    if rows.is_empty() {
        return Ok(());
    }
    // The legacy single table predates both sharding AND RecordId keying: its rows
    // are `(db, coll, id_key) -> raw blob`. Re-key each into its shard by a fresh
    // RecordId (framed value carries the id_key), and write the `_id` index row
    // (id_key -> RecordId). A global counter keeps RecordIds unique per collection;
    // `scan_max_nat_seq` (run just after, in `load_oplog_meta`) recovers the
    // counter from the shards so freshly minted seqs stay strictly greater.
    let idx = session.open_cursor(NAT_SEQ_TABLE, None)?;
    for (recordid, (db, coll, id_key, blob)) in (1i64..).zip(&rows) {
        let dst = session.open_cursor(&doc_table_for(db, coll), None)?;
        dst.set_key_ssq(db, coll, recordid);
        dst.set_value_u(&frame_doc_value(id_key, blob));
        dst.insert()?;
        idx.reset()?;
        idx.set_key_ssu(db, coll, id_key);
        idx.set_value_q(recordid);
        idx.insert()?;
    }
    let del = session.open_cursor(DOC_TABLE, None)?;
    for (db, coll, id_key, _) in &rows {
        del.reset()?;
        del.set_key_ssu(db, coll, id_key);
        match del.remove() {
            Ok(()) => {}
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Parse the `key_format=<fmt>` value out of a WiredTiger config / `metadata:`
/// string (`fmt` is a simple token — `SSq`, `SSu`, … — so it stops at the next
/// comma). Returns `None` if the string has no `key_format` clause.
fn extract_key_format(cfg: &str) -> Option<&str> {
    let start = cfg.find("key_format=")? + "key_format=".len();
    let rest = &cfg[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Refuse to open a store whose document shards were written by a build BEFORE
/// the RecordId doc-table change (see `tasks/backlog.md` §7.8). Those tables are
/// keyed `SSu` (`(db, coll, id_key)`) with unframed blob values; this build keys
/// them `SSq` (`(db, coll, RecordId)`) with framed values (see [`frame_doc_value`]).
/// WiredTiger fixes a table's `key_format` at CREATE time and preserves it across
/// reopen — the bootstrap `create` is a no-op for an existing table, which is
/// exactly what lets [`migrate_legacy_docs`] read the legacy table as `SSu` — so
/// the on-disk schema read here from the `metadata:` cursor is the ground truth.
///
/// There is deliberately **no in-place migration**: the two Rust servers are
/// pre-1.0 beta with no upgrading users, so the correct response to an
/// incompatible on-disk format is to refuse to open rather than silently mis-read
/// stored data with `SSq` cursor ops against an `SSu` btree. (A pre-*shard* store
/// is the separate, supported case — its legacy single `secantus_documents` table
/// is folded in by [`migrate_legacy_docs`] — so only the sharded doc tables are
/// inspected here.)
fn reject_pre_recordid_doc_format(session: &Session) -> Result<()> {
    let meta = session.open_cursor("metadata:", None)?;
    for s in 0..DOC_SHARDS {
        let name = doc_shard_name(s);
        meta.reset()?;
        meta.set_key_s(&name);
        if meta.search().is_ok() {
            let cfg = meta.get_value_s()?;
            if extract_key_format(&cfg) == Some("SSu") {
                return Err(StorageError::Internal(format!(
                    "SecantusDB storage at this path was written by a build before \
                     the RecordId doc-table change: '{name}' is keyed 'SSu' but this \
                     build requires 'SSq'. There is no in-place upgrade (pre-1.0 \
                     beta, no migration) — start from a fresh data directory or \
                     downgrade to the build that wrote it."
                )));
            }
        }
    }
    Ok(())
}

/// Refuse to open a store whose index entries predate the RecordId entry format
/// (step 2). Those entries carry the doc's `id_key` in their trailing half; this
/// build reads that half as an 8-byte RecordId. Unlike the step-1 doc-table
/// change this is NOT visible in WiredTiger's `key_format` (still `SSSu` either
/// way) — the difference is inside the value bytes — so the index catalog
/// carries an explicit `options.entryFormat` marker and its absence is the
/// signal.
///
/// There is deliberately **no migration**: the Rust servers are pre-1.0 beta
/// with no upgrading users, so refusing to open beats re-packing every index
/// entry on a path that has to be perfect. (`unpack_entry` already returns
/// `None` for a legacy entry rather than mis-reading it, so nothing fetches the
/// wrong document even before this fires — this turns a silent
/// nothing-matches into a loud refusal.)
fn reject_legacy_index_entry_format(session: &Session) -> Result<()> {
    let c = match session.open_cursor(IDX_TABLE, None) {
        Ok(c) => c,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut more = c.next()?;
    while more {
        let (db, coll, name) = c.get_key_sss()?;
        let blob = c.get_value_u()?;
        if !blob.is_empty() {
            let d = decode_doc(&blob)?;
            let fmt = d
                .get_document("options")
                .ok()
                .and_then(|o| o.get_i32("entryFormat").ok())
                .unwrap_or(1);
            if fmt < ENTRY_FORMAT_RECORDID {
                return Err(StorageError::Internal(format!(
                    "SecantusDB storage at this path has index entries written by a \
                     build before the RecordId index-entry change: index '{name}' on \
                     '{db}.{coll}' is entryFormat {fmt}, but this build requires \
                     {ENTRY_FORMAT_RECORDID}. There is no in-place upgrade (pre-1.0 \
                     beta, no migration) — start from a fresh data directory, drop and \
                     recreate the indexes, or downgrade to the build that wrote it."
                )));
            }
        }
        more = c.next()?;
    }
    Ok(())
}

const IDX_TABLE: &str = "table:secantus_indexes";
const IDX_ENTRIES_TABLE: &str = "table:secantus_index_entries";

/// Ceiling on the number of keys one doc may contribute to a compound index
/// when more than one indexed field is array-valued (the cartesian product).
const MAX_COMPOUND_KEYS: usize = 10_000;

// Natural-order (insertion) index. mongod returns an unsorted `find()` in
// insertion (storage / RecordId) order, which equals `_id` order only for
// monotonic `_id`s — so a separate seq index is needed. `NAT_TABLE` maps a
// monotonic insertion `seq` to the doc's `id_key`; `NAT_SEQ_TABLE` is the reverse
// (so a delete can find and drop the seq). Mirrors the Python storage's
// `secantus_natural` / `secantus_natural_seq`.
const NAT_TABLE: &str = "table:secantus_natural";
const NAT_SEQ_TABLE: &str = "table:secantus_natural_seq";

// Oplog / change-stream tables (Phase 4 sub-phase 3). `q`-keyed (int64 seq) for
// the oplog + pre-images; a single `S` key ("state") for the recovery metadata.
/// Legacy single-table oplog name. Retained only for the pre-shard on-disk
/// upgrade read (a store written by an older build still has its entries here);
/// all new writes go to the shard tables. See `OPLOG_SHARDS`.
const OPLOG_TABLE: &str = "table:secantus_oplog";
/// The oplog is sharded across `OPLOG_SHARDS` btrees so concurrent writers don't
/// all rendezvous on one table's rightmost append page — the A/B-proven scaling
/// bottleneck (a single append point serialised every writer; sharding recovered
/// 8-writer scaling 0.60x -> 2.47x and single-writer 16k -> 28.9k docs/s, both
/// approaching the oplog-off ceiling; `tasks/rust-mongodb-parity-redesign.md`).
///
/// Routing is **per batch**: a whole `emit_oplog` call's run of seqs goes to one
/// shard (`start_seq % OPLOG_SHARDS`). This is deliberate — routing per *entry*
/// (`seq % N`) scatters a 100-doc batch's contiguous seqs across all N btrees,
/// destroying the sequential-append locality even the single table had (measured:
/// per-entry regressed single-writer to ~8.6k docs/s, *below* the 16k baseline).
/// Per-batch keeps each batch a contiguous append to one tree (fast) while
/// concurrent writers, minting different `start_seq`s, spread across trees (the
/// scaling win). The cost: a seq's shard is NOT a function of the seq, so ordered
/// reads use a k-way merge (`read_oplog_shards`) and per-seq point-ops
/// (prune-delete via the merge's shard tag; archive / recovery-ts) consider all
/// tables.
const OPLOG_SHARDS: i64 = 16;

/// Shard table name for shard index `shard` (0..OPLOG_SHARDS).
fn oplog_shard_name(shard: i64) -> String {
    format!("table:secantus_oplog_sh{shard}")
}

/// `SECANTUS_OPLOG_NONLOGGED=1` creates the oplog + preimage tables with WAL
/// logging disabled (`log=(enabled=false)`): oplog rows become checkpoint-durable
/// only — a hard crash loses the tail written since the last checkpoint (data
/// tables stay fully logged; a clean close checkpoints everything). This removes
/// the oplog's share of WAL bandwidth, the dominant write-path cost under
/// concurrent load (`tasks/rust-perf-findings.md` Finding 5). Applies at table
/// CREATE time — an existing store keeps whatever its tables were created with.
fn oplog_tables_nonlogged() -> bool {
    std::env::var_os("SECANTUS_OPLOG_NONLOGGED").is_some()
}

/// Table-create config for the oplog shard / legacy-oplog / preimage tables,
/// honouring [`oplog_tables_nonlogged`].
fn oplog_table_cfg() -> String {
    if oplog_tables_nonlogged() {
        format!("{QU_COMPRESSED_CFG},log=(enabled=false)")
    } else {
        QU_COMPRESSED_CFG.to_string()
    }
}

/// The shard table a batch starting at `start_seq` is routed to. `rem_euclid`
/// keeps it correct for any i64 (seqs are always >= 1 in practice, but never
/// route to a negative index).
fn oplog_shard_for_batch(start_seq: i64) -> String {
    oplog_shard_name(start_seq.rem_euclid(OPLOG_SHARDS))
}

/// Merge-read the sharded oplog in ascending seq order, starting at the first
/// seq >= `start_seq`, up to `limit` non-empty entries. A k-way merge across the
/// `OPLOG_SHARDS` shard cursors (each shard is seq-sorted): repeatedly emit the
/// smallest head seq and advance that shard. Handles gaps in the seq space
/// (prune removes a low prefix; a rolled-back mint would leave a hole) — it never
/// assumes contiguity, so a missing seq can't truncate a change-stream read.
/// Empty-blob rows (defensive) are skipped but still advance, exactly like the
/// old single-table walk.
fn read_oplog_shards(
    session: &Session,
    start_seq: i64,
    limit: usize,
) -> Result<Vec<(i64, Vec<u8>)>> {
    Ok(read_oplog_shards_tagged(session, start_seq, limit)?
        .into_iter()
        .map(|(seq, _tbl, blob)| (seq, blob))
        .collect())
}

/// Every table an oplog reader / point-op must consider: the N shards followed by
/// the legacy single table. A batch's whole run of seqs lives in one of these
/// (routing is per-batch, so a seq's shard is NOT derivable from the seq — hence
/// the merge and the all-table point-ops).
fn oplog_all_tables() -> Vec<String> {
    (0..OPLOG_SHARDS)
        .map(oplog_shard_name)
        .chain(std::iter::once(OPLOG_TABLE.to_string()))
        .collect()
}

/// Like `read_oplog_shards` but also returns each row's source table index into
/// `oplog_all_tables()` (0..OPLOG_SHARDS = shard, OPLOG_SHARDS = legacy). Prune
/// uses the tag to delete each doomed row from its exact table instead of probing
/// all of them.
fn read_oplog_shards_tagged(
    session: &Session,
    start_seq: i64,
    limit: usize,
) -> Result<Vec<(i64, usize, Vec<u8>)>> {
    // seq ranges across tables are disjoint (recovery clamps new seqs strictly
    // above any legacy seq), so a plain min-seq merge needs no dedup.
    let tables = oplog_all_tables();
    let mut cursors: Vec<Cursor> = Vec::with_capacity(tables.len());
    // Current head seq of each shard cursor, or None once exhausted.
    let mut heads: Vec<Option<i64>> = Vec::with_capacity(tables.len());
    for tbl in &tables {
        let cur = session.open_cursor(tbl, None)?;
        cur.set_key_q(start_seq);
        let positioned = match cur.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    // search_near landed just below start_seq; step to the first >=.
                    cur.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        heads.push(if positioned {
            Some(cur.get_key_q()?)
        } else {
            None
        });
        cursors.push(cur);
    }
    let mut out: Vec<(i64, usize, Vec<u8>)> = Vec::new();
    while out.len() < limit {
        // Pick the shard whose head is the smallest seq.
        let mut best: Option<usize> = None;
        let mut best_seq = i64::MAX;
        for (i, h) in heads.iter().enumerate() {
            if let Some(seq) = *h {
                if seq < best_seq {
                    best_seq = seq;
                    best = Some(i);
                }
            }
        }
        let Some(i) = best else { break };
        let blob = cursors[i].get_value_u()?;
        if !blob.is_empty() {
            out.push((best_seq, i, blob));
        }
        heads[i] = if cursors[i].next()? {
            Some(cursors[i].get_key_q()?)
        } else {
            None
        };
    }
    Ok(out)
}

/// Largest seq present across all oplog shards (0 if all empty). A single
/// `prev()` from each shard's end lands on that shard's max; the answer is the
/// max of those. Includes the legacy single table so recovery of a pre-shard
/// store still finds its tail.
fn scan_max_oplog_seq(session: &Session) -> i64 {
    let mut max = 0i64;
    for s in 0..OPLOG_SHARDS {
        if let Ok(c) = session.open_cursor(&oplog_shard_name(s), None) {
            if c.prev().unwrap_or(false) {
                max = max.max(c.get_key_q().unwrap_or(0));
            }
        }
    }
    if let Ok(c) = session.open_cursor(OPLOG_TABLE, None) {
        if c.prev().unwrap_or(false) {
            max = max.max(c.get_key_q().unwrap_or(0));
        }
    }
    max
}

/// Total live oplog rows across all shards + the legacy table. Key-only walk (no
/// value fetch), run once on open to seed `OplogState.live_count` so the
/// opportunistic prune can early-out instead of re-scanning the whole oplog.
fn count_oplog_entries(session: &Session) -> i64 {
    let mut total = 0i64;
    for tbl in oplog_all_tables() {
        if let Ok(c) = session.open_cursor(&tbl, None) {
            while c.next().unwrap_or(false) {
                total += 1;
            }
        }
    }
    total
}
const PREIMAGE_TABLE: &str = "table:secantus_preimages";
const OPLOG_META_TABLE: &str = "table:secantus_oplog_meta";

// Auth / profiling tables. Users + roles are `(db, name) -> bson(record)` (key
// `SS`); per-database profile settings are `db -> bson({level, slowms,
// sampleRate})` (key `S`). Records are opaque BSON blobs (the command layer owns
// their shape), stored / returned verbatim across the byte seam.
const USERS_TABLE: &str = "table:secantus_users";
const ROLES_TABLE: &str = "table:secantus_roles";
const PROFILE_TABLE: &str = "table:secantus_profile_settings";

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

/// `(index_name, direction, is_compound)` — the index a leading-field lookup
/// resolves to (the tuple `find_leading_field_index` returns).
type LeadingFieldMatch = (String, i32, bool);

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

/// The result of an `update_matching` call (mirrors the `{matched, modified,
/// upserted_id}` dict `storage.update_matching` returns).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateOutcome {
    /// Documents that matched the filter (regardless of whether they changed).
    pub matched: usize,
    /// Documents actually rewritten (matched *and* the update changed them).
    pub modified: usize,
    /// The `_id` inserted by an `upsert` when nothing matched, else `None`.
    pub upserted_id: Option<Bson>,
    /// The post-image of the written document for a single-doc
    /// (`multi == false`) update or an upsert, captured while the statement
    /// holds the storage lock. `findAndModify {new: true}` returns this —
    /// a post-write re-`find` is a separate call a concurrent writer can land
    /// in front of, handing two clients the same "new" document. `None` for
    /// `multi` updates and when nothing matched.
    pub post_image: Option<Document>,
}

/// A stored index, with the options the write / lookup paths care about,
/// parsed out of its registry `options` blob.
struct IndexDesc {
    name: String,
    key_spec: Document,
    sparse: bool,
    unique: bool,
    /// `prepareUnique` (set via `collMod`): enforce uniqueness on *new* writes
    /// (block dup inserts with 11000) while pre-existing duplicates are tolerated
    /// — the staging step before a `unique: true` conversion.
    prepare_unique: bool,
    /// `partialFilterExpression` if non-empty — entries are written (and the
    /// index considered) only for docs/queries that match / imply it.
    partial: Option<Document>,
    /// `Some` for a `2d` geohash index — its field + bucketing params. Geo
    /// indexes use a separate point-only entry scheme, not `index_key_variants`.
    geo_2d: Option<Geo2d>,
    /// `Some` for a `2dsphere` S2 index — its field. Writes covering cells +
    /// ancestors per geometry, not `index_key_variants`.
    geo_sphere: Option<GeoSphere>,
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
    // Like parse_geo_sphere: any field valued "2d" makes this a 2d index on that
    // field; compound geo+scalar specs are accepted as geo-only (trailing scalar
    // fields ignored at index time). Mirrors storage._geo_type_of.
    let (field, _) = key_spec.iter().find(|(_, v)| v.as_str() == Some("2d"))?;
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

/// A `2dsphere` index's parameters — just the field. Spherical indexes use S2
/// cell coverings: each indexed geometry writes its covering cells *plus every
/// ancestor back to level 0* (mirroring real-mongo's S2 scheme and
/// `geo_index.s2_doc_covering`), so a query covering at any level finds it.
#[derive(Clone)]
struct GeoSphere {
    field: String,
}

impl GeoSphere {
    /// The S2 cell key bytes (8-byte big-endian, covering + ancestors) for a
    /// doc field value: a point covers its leaf cell + ancestors; any other
    /// geometry covers its bounding rect (over-covers; the `matches()` verifier
    /// filters false positives). Empty when the value isn't a geometry.
    fn cell_kbs(&self, value: &Bson) -> Vec<Vec<u8>> {
        let cells = if let Some((x, y)) = secantus_core::geo::doc_point(value) {
            s2_cells_for_point(x, y)
        } else if let Some((min_x, min_y, max_x, max_y)) = secantus_core::geo::doc_bbox(value) {
            s2_cells_for_bbox(min_x, min_y, max_x, max_y)
        } else {
            return Vec::new();
        };
        cells
            .into_iter()
            .map(|c| secantus_core::geo::encode_cell(c).to_vec())
            .collect()
    }
}

/// Parse a `2dsphere` geo index from its key spec (`{field: "2dsphere"}`).
/// `None` if it isn't a single-field `2dsphere` index.
fn parse_geo_sphere(key_spec: &Document) -> Option<GeoSphere> {
    // A 2dsphere index is any spec containing a field whose value is the string
    // "2dsphere". Compound geo+scalar specs ({g:"2dsphere", z:1}) are accepted as
    // geo-only on the geo field; trailing scalar fields are ignored at index time
    // and verified post-fetch (mirrors storage._geo_type_of).
    let (field, _) = key_spec
        .iter()
        .find(|(_, v)| v.as_str() == Some("2dsphere"))?;
    Some(GeoSphere {
        field: field.clone(),
    })
}

/// The S2 region coverer SecantusDB uses for `2dsphere` coverings (mirrors
/// `geo_index._make_coverer`: min level 4, max level 16, 64 cells, level_mod 1).
fn s2_coverer() -> RegionCoverer {
    RegionCoverer {
        min_level: 4,
        max_level: 16,
        level_mod: 1,
        max_cells: 64,
    }
}

/// Append `cell` and every ancestor up to level 0 to `out` (deduped via `seen`).
/// Mirrors `geo_index._cell_with_ancestors`.
fn cell_with_ancestors(cell: CellID, out: &mut Vec<u64>, seen: &mut HashSet<u64>) {
    let mut c = cell;
    loop {
        if seen.insert(c.0) {
            out.push(c.0);
        }
        let lvl = c.level();
        if lvl == 0 {
            break;
        }
        c = c.parent(lvl - 1);
    }
}

/// S2 cells (covering + ancestors) for a point at `(lng, lat)` — its leaf cell
/// plus every ancestor. Mirrors `geo_index.s2_doc_covering` for a `Point`.
fn s2_cells_for_point(lng: f64, lat: f64) -> Vec<u64> {
    let leaf = CellID::from(LatLng::from_degrees(lat, lng));
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    cell_with_ancestors(leaf, &mut out, &mut seen);
    out
}

/// S2 cells (covering + ancestors) for a lng/lat bounding box. Covers the
/// `LatLngRect` (s2sphere covers shapes via their bounding rect — over-covers,
/// filtered later) and expands every covering cell to its ancestors.
fn s2_cells_for_bbox(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Vec<u64> {
    let rect = Rect::from_degrees(min_y, min_x, max_y, max_x);
    let union = s2_coverer().covering(&rect);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for c in union.0 {
        cell_with_ancestors(c, &mut out, &mut seen);
    }
    out
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
///
/// `prealloc=false` disables WT's log-file pre-allocation. By default WT's log
/// server keeps two `file_max`-sized `WiredTigerPreplog` files ready ahead of
/// the active log, so with `file_max=128MB` every on-disk instance costs
/// ~256 MB of pre-sized journal space even for a database holding a few KB.
/// That pre-allocation is a write-latency optimisation for sustained-throughput
/// servers; SecantusDB's instances are small and short-lived (a full test run
/// spins up thousands), so the latency win is irrelevant and the disk cost is
/// not — it was exhausting the small Windows CI runner disk with 128 MB
/// `WiredTigerTmplog` files. Disabling prealloc drops each instance's idle log
/// footprint to what it actually writes (WT grows the active segment on demand)
/// with no durability change — recovery still replays the same log records.
/// This matches `storage.py`, which ships `prealloc=false` in production.
/// `file_max=128MB` is kept (see PR #575, mongod-parity Phase 1): with prealloc
/// off, a production sustained-writer still gets 128 MB active segments while
/// tiny test DBs cost only what they write.
const DEFAULT_CONFIG: &str = "create,session_max=1000,cache_size=256M,\
                              eviction=(threads_min=4,threads_max=4),\
                              log=(enabled=true,file_max=128MB,prealloc=false),\
                              transaction_sync=(enabled=false,method=fsync)";

/// Build the WiredTiger connection config string from the tunable knobs the
/// `secantusdb` daemon exposes (`--cache-size`, `--session-max`,
/// `--sync-on-commit`). Mirrors `storage.py`'s config assembly and matches
/// [`DEFAULT_CONFIG`] byte-for-byte for the engine defaults (`"256M"`, `1000`,
/// `false`) — see the `wt_config_matches_default` test. The `secantusdb`
/// binary passes the resolved `cache_size` (Python's default is `"1G"`), while
/// the embedded / library default stays `256M` via [`Storage::open`].
pub fn wt_config(
    cache_size: &str,
    session_max: u32,
    sync_on_commit: bool,
    log_file_max: &str,
) -> String {
    let mut cfg = format!(
        "create,session_max={session_max},cache_size={cache_size},eviction=(threads_min=4,threads_max=4),log=(enabled=true,file_max={log_file_max},prealloc=false),transaction_sync=(enabled={},method=fsync)",
        if sync_on_commit { "true" } else { "false" }
    );
    // `SECANTUS_WT_CONFIG_EXTRA` appends raw WiredTiger connection config — a
    // tuning / experiment hook (e.g. `eviction=(threads_min=8,threads_max=8)` or
    // `cache_size=4G`). WiredTiger's parser takes the LAST occurrence of a
    // duplicated key, so an appended clause overrides the corresponding default.
    if let Some(extra) = std::env::var_os("SECANTUS_WT_CONFIG_EXTRA") {
        if let Some(extra) = extra.to_str() {
            let extra = extra.trim();
            if !extra.is_empty() {
                cfg.push(',');
                cfg.push_str(extra);
            }
        }
    }
    cfg
}

// zlib block compression on the value-heavy tables (document blobs live in the
// doc / oplog / preimage tables) — cuts the per-doc disk-write volume that bounds
// steady-state write throughput (measured +25% single-writer and ~2× multi-writer
// scaling on compressible data), mirroring mongod's compress-by-default. The WT
// build links the builtin zlib extension (HAVE_BUILTIN_EXTENSION_ZLIB) on macOS +
// Linux, where libz is present; **Windows** WT is built without it (no default
// libz), so the compressor clause is omitted there — a `block_compressor=zlib`
// table create would fail with "unknown compressor". Set at create time; existing
// uncompressed tables keep their format (WT stores it in metadata).
// RecordId keying: the doc table is keyed by (db, coll, RecordId:i64) — the
// monotonic per-collection insertion seq — not by id_key. This puts the table in
// insertion order (so the `secantus_natural` forward table is dropped) and cuts
// write amplification 4->3. `secantus_natural_seq` (id_key -> RecordId) is the
// `_id` index. See tasks/rust-recordid-plan.md.
#[cfg(not(target_os = "windows"))]
const DOC_TABLE_CFG: &str = "key_format=SSq,value_format=u,block_compressor=zlib";
#[cfg(target_os = "windows")]
const DOC_TABLE_CFG: &str = "key_format=SSq,value_format=u";
#[cfg(not(target_os = "windows"))]
const QU_COMPRESSED_CFG: &str = "key_format=q,value_format=u,block_compressor=zlib";
#[cfg(target_os = "windows")]
const QU_COMPRESSED_CFG: &str = "key_format=q,value_format=u";

// The full table set `storage.py` bootstraps. Sub-phase 1 only reads/writes the
// collections + documents tables, but creating the rest keeps the on-disk schema
// identical so later sub-phases don't need a migration.
const BOOTSTRAP: &[(&str, &str)] = &[
    (COLL_TABLE, "key_format=SS,value_format=u"),
    (DOC_TABLE, DOC_TABLE_CFG),
    ("table:secantus_indexes", "key_format=SSS,value_format=u"),
    (
        "table:secantus_index_entries",
        "key_format=SSSu,value_format=u",
    ),
    ("table:secantus_natural", "key_format=SSq,value_format=u"),
    (
        "table:secantus_natural_seq",
        "key_format=SSu,value_format=q",
    ),
    // NOTE: table:secantus_oplog + table:secantus_preimages are created in
    // `open_with_config_durable` (their config depends on `oplog_table_cfg()`).
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
    /// `create_index` was asked to re-create an existing index *name* with a
    /// different key spec.
    IndexKeySpecsConflict(String),
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
    /// An internal invariant failure (e.g. a transaction operation on an
    /// already-closed handle). Surfaces as a command-level `InternalError`.
    Internal(String),
    /// A write lost a `WT_ROLLBACK` race (write conflict). Surfaces as mongod's
    /// `WriteConflict` (112); inside a transaction it also earns the
    /// `TransientTransactionError` label so drivers retry the whole transaction.
    WriteConflict,
    /// A document's encoded size exceeds `MAX_BSON_OBJECT_SIZE`. Carries the
    /// offending size; surfaces as mongod's `BSONObjectTooLarge` (10334).
    DocumentTooLarge(usize),
    /// The post-apply document failed the collection `validator`. Surfaces as
    /// mongod's `DocumentValidationFailure` (121).
    DocumentValidationFailure,
    /// An update would modify the immutable `_id` field. Surfaces as mongod's
    /// `ImmutableField` (66).
    ImmutableField,
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
            StorageError::IndexKeySpecsConflict(m) => write!(f, "{m}"),
            StorageError::QueryUnsupported => {
                write!(f, "query construct not supported by the Rust query engine")
            }
            StorageError::ImmutableField => write!(
                f,
                "Performing an update on the path '_id' would modify the immutable field '_id'"
            ),
            StorageError::BadHint(m) => write!(f, "{m}"),
            StorageError::ChangeStreamFatal(m) => write!(f, "{m}"),
            StorageError::Internal(m) => write!(f, "{m}"),
            StorageError::WriteConflict => write!(
                f,
                "WriteConflict error: this operation conflicted with another operation"
            ),
            StorageError::DocumentTooLarge(size) => write!(
                f,
                "object to insert too large. size in bytes: {size}, max size: {MAX_BSON_OBJECT_SIZE}"
            ),
            StorageError::DocumentValidationFailure => write!(f, "Document failed validation"),
        }
    }
}
impl std::error::Error for StorageError {}
impl From<WtError> for StorageError {
    fn from(e: WtError) -> Self {
        // A `WT_ROLLBACK` means the write lost a concurrency race — surface it as
        // a dedicated `WriteConflict` so the command layer can map it to mongod's
        // 112 (+ the transient label inside a transaction) rather than a generic
        // internal error.
        if e.is_rollback() {
            StorageError::WriteConflict
        } else {
            StorageError::Wt(e)
        }
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

fn encode_doc(doc: &Document) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    // mongod stores `_id` as the FIRST field of every document. Replacement /
    // upsert updates can leave `_id` elsewhere in field order; reorder it to the
    // front so the stored (and later returned) document matches mongod exactly —
    // order-sensitive driver comparisons (e.g. the C# CRUD-spec runner) check
    // this. A no-op for docs without a top-level `_id` (oplog / index / meta
    // blobs) or where `_id` is already first.
    let needs_reorder =
        doc.contains_key("_id") && doc.keys().next().map(String::as_str) != Some("_id");
    if needs_reorder {
        let mut ordered = Document::new();
        if let Some(id) = doc.get("_id") {
            ordered.insert("_id", id.clone());
        }
        for (k, v) in doc {
            if k != "_id" {
                ordered.insert(k.clone(), v.clone());
            }
        }
        ordered
            .to_writer(&mut buf)
            .map_err(|e| StorageError::Bson(e.to_string()))?;
        return Ok(buf);
    }
    doc.to_writer(&mut buf)
        .map_err(|e| StorageError::Bson(e.to_string()))?;
    Ok(buf)
}

fn decode_doc(bytes: &[u8]) -> Result<Document> {
    Document::from_reader(&mut std::io::Cursor::new(bytes))
        .map_err(|e| StorageError::Bson(e.to_string()))
}

/// Encode a `{_id: <id>}` document — the oplog `o2` for every CRUD op (and the
/// `o` of a `delete`, which mongod records as just the deleted doc's `_id`).
fn encode_id_doc(id: &Bson) -> Result<Vec<u8>> {
    let mut d = Document::new();
    d.insert("_id", id.clone());
    encode_doc(&d)
}

/// `id_key = sortkey.encode_value(_id)` — the byte-sortable key for the `_id`.
fn id_key(id: &Bson) -> Result<Vec<u8>> {
    sortkey::encode_value(id, None).map_err(|_| StorageError::UnsupportedId)
}

/// Whether applying `update` to a doc with `_id == old_id` would modify the
/// immutable `_id` field (mongod rejects this with `ImmutableField`, code 66).
/// Operator-form: any modifier touching `_id` (or an `_id.` sub-path), except a
/// `$set`/`$setOnInsert` that sets `_id` to its current value. Replacement-form:
/// a specified `_id` different from the current one.
fn update_would_change_id(update: &Document, old_id: &Bson) -> bool {
    let touches_id = |fields: &Document| fields.keys().any(|k| k == "_id" || k.starts_with("_id."));
    if !update.keys().any(|k| k.starts_with('$')) {
        // Replacement-style: only a *different* explicit `_id` is a violation.
        return matches!(update.get("_id"), Some(v) if v != old_id);
    }
    for (op, arg) in update {
        let Some(fields) = arg.as_document() else {
            continue;
        };
        match op.as_str() {
            "$set" | "$setOnInsert" => {
                if let Some(v) = fields.get("_id") {
                    if v != old_id {
                        return true;
                    }
                }
                if fields.keys().any(|k| k.starts_with("_id.")) {
                    return true;
                }
            }
            "$unset" | "$inc" | "$mul" | "$min" | "$max" | "$pop" | "$push" | "$pull"
            | "$pullAll" | "$addToSet" | "$bit" | "$currentDate" => {
                if touches_id(fields) {
                    return true;
                }
            }
            "$rename" => {
                for (from, to) in fields {
                    if from == "_id" || from.starts_with("_id.") {
                        return true;
                    }
                    if let Bson::String(t) = to {
                        if t == "_id" || t.starts_with("_id.") {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Resolve `$currentDate` to concrete clock values so the deterministic core
/// update engine (which defers `$currentDate` as non-deterministic) never sees
/// it. Each field is folded into `$set`: a `true` value or `{$type: "date"}`
/// becomes the current UTC `DateTime`; `{$type: "timestamp"}` becomes
/// `Timestamp(now_secs, 0)`. All fields in one update share a single clock read
/// (mongod applies one operation time). The update is returned unchanged when it
/// carries no `$currentDate`. Mirrors `update.py`'s `$currentDate` branch. An
/// unrecognised option (anything other than `true` / `{$type: "date"|"timestamp"}`)
/// or a non-document `$set`/`$currentDate` is rejected.
fn resolve_current_date(update: &Document) -> Result<Document> {
    let Some(cd_bson) = update.get("$currentDate") else {
        return Ok(update.clone());
    };
    let Bson::Document(cd) = cd_bson else {
        return Err(StorageError::QueryUnsupported);
    };
    let millis = now_millis();
    let date = Bson::DateTime(bson::DateTime::from_millis(millis));
    let ts = Bson::Timestamp(bson::Timestamp {
        time: (millis / 1000) as u32,
        increment: 0,
    });
    let mut out = update.clone();
    out.remove("$currentDate");
    let mut set = match out.remove("$set") {
        Some(Bson::Document(d)) => d,
        None => Document::new(),
        Some(_) => return Err(StorageError::QueryUnsupported),
    };
    for (path, opts) in cd {
        let value = match opts {
            // A boolean (true OR false) sets the current Date, matching mongod
            // and the Python `$currentDate` branch.
            Bson::Boolean(_) => date.clone(),
            Bson::Document(o) => match o.get_str("$type") {
                Ok("date") => date.clone(),
                Ok("timestamp") => ts.clone(),
                _ => return Err(StorageError::QueryUnsupported),
            },
            _ => return Err(StorageError::QueryUnsupported),
        };
        set.insert(path.clone(), value);
    }
    out.insert("$set", Bson::Document(set));
    Ok(out)
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

/// Whether two index key specs name the *same* index. Field names and order must
/// match exactly (mongod treats `{a:1,b:1}` and `{b:1,a:1}` as distinct), but
/// numeric direction values compare numerically — `{a: 1}` and `{a: 1.0}` are the
/// same index (drivers such as mongocxx's GridFS create indexes with `1.0`).
/// Non-numeric directions (geo type strings like `"2dsphere"`) compare exactly.
fn index_keys_equiv(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|((ak, av), (bk, bv))| {
            ak == bk
                && match (direction_of(av), direction_of(bv)) {
                    (Some(da), Some(db)) => da == db,
                    _ => av == bv,
                }
        })
}

/// Format a scalar BSON value the way the mongo shell prints it, for the
/// `dup key: { … }` fragment of an E11000 message (drivers like the PHP extension
/// pin the text verbatim). Mirrors `storage._shell_value`.
fn shell_value(v: &Bson) -> String {
    match v {
        Bson::Boolean(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Bson::String(s) => format!("\"{s}\""),
        Bson::ObjectId(o) => format!("ObjectId('{o}')"),
        Bson::Null => "null".to_string(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        // Match Python `str(float)`: integral doubles keep a trailing `.0`.
        Bson::Double(d) if d.fract() == 0.0 && d.is_finite() => format!("{d:.1}"),
        Bson::Double(d) => d.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        other => format!("{other:?}"),
    }
}

/// Build mongod's E11000 duplicate-key `errmsg`:
/// `E11000 duplicate key error collection: <ns> index: <name> dup key: { <k>: <v>, … }`
/// — the exact shape drivers assert against. Mirrors `storage.format_dup_key_errmsg`.
fn format_dup_key_errmsg(namespace: &str, index_name: &str, key_value: &Document) -> String {
    let dup = if key_value.is_empty() {
        "{ }".to_string()
    } else {
        let inner: Vec<String> = key_value
            .iter()
            .map(|(k, v)| format!("{k}: {}", shell_value(v)))
            .collect();
        format!("{{ {} }}", inner.join(", "))
    };
    format!("E11000 duplicate key error collection: {namespace} index: {index_name} dup key: {dup}")
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

/// On-disk index-ENTRY format version, recorded per index as
/// `options.entryFormat` in the index catalog. 1 (implicit, absent) = step-1
/// entries whose trailing half is the doc's `id_key`; 2 = step-2 entries whose
/// trailing half is the 8-byte RecordId. The catalog is the only place this is
/// visible — the WT `key_format` is `SSSu` either way — so an absent marker is
/// how a legacy store is detected (`reject_legacy_index_entry_format`).
const ENTRY_FORMAT_RECORDID: i32 = 2;

/// Pack an index-entry payload into a single trailing `u` column:
/// `escape(kb) + b"\x00\x00" + RecordId(8B big-endian)`. WiredTiger
/// length-prefixes non-trailing `u` columns, which would break lexicographic
/// order — so both halves live in one column and the B-tree sorts by
/// `escape(kb)` first, then by RecordId.
///
/// **Step 2 format (`ENTRY_FORMAT_RECORDID`).** The trailing half used to be the
/// doc's `id_key`, which made an IXSCAN fetch pay `id_key → _id index → RecordId
/// → doc`. Storing the RecordId directly drops that hop (measured at +14.7% on
/// `find_indexed_range` — see `tasks/rust-recordid-plan.md`). Big-endian is
/// deliberate: it keeps the B-tree ordering within one key in RecordId
/// (insertion) order, and it is fixed-width so the trailing half needs no
/// escaping even though a RecordId's bytes may themselves contain `\x00\x00`
/// (`unpack_entry` splits at the FIRST separator, and the escaped `kb` half
/// cannot contain one).
fn pack_entry(kb: &[u8], recordid: i64) -> Vec<u8> {
    let mut out = escape_kb(kb);
    out.extend_from_slice(ENTRY_SEP);
    out.extend_from_slice(&recordid.to_be_bytes());
    out
}

/// Split a packed entry into `(escaped_kb, RecordId)` at the FIRST `\x00\x00`.
/// Correct because the `kb` half is escaped (no bare `\x00\x00` can occur in
/// it). A trailing half that is not exactly 8 bytes is a step-1-format entry;
/// callers must never see one (`reject_legacy_index_entry_format` refuses such a
/// store at open), so it is reported as `None` rather than silently mis-read.
fn unpack_entry(packed: &[u8]) -> (&[u8], Option<i64>) {
    match packed.windows(2).position(|w| w == ENTRY_SEP) {
        Some(i) => {
            let tail = &packed[i + 2..];
            let rid = <[u8; 8]>::try_from(tail).ok().map(i64::from_be_bytes);
            (&packed[..i], rid)
        }
        None => (packed, None),
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

/// Candidate index values for `field` in `doc`, plus whether the field makes
/// the index multikey. Mirrors mongod's key generation: a scalar leaf gives
/// itself; an array leaf gives one value per element *plus* the whole array
/// (the key a whole-array equality probes); a path descending *through* an
/// array — `prices.owner_id` over an array of subdocuments — gives one value
/// per element's leaf and no whole-array key. A missing path indexes as null.
/// Mirrors `storage._index_field_values`.
fn index_field_values(doc: &Document, field: &str) -> (Vec<Bson>, bool) {
    let (values, descended) = get_path_values(doc, field);
    if values.is_empty() {
        return (vec![Bson::Null], descended);
    }
    let mut out: Vec<Bson> = Vec::new();
    let mut multikey = descended;
    for v in values {
        if let Bson::Array(arr) = v {
            multikey = true;
            out.extend(arr.iter().cloned());
            out.push(v.clone());
        } else {
            out.push(v.clone());
        }
    }
    (out, multikey)
}

/// `has_path` that descends into arrays — the sparse-index gate. A sparse index
/// must cover `{"prices": [{"owner": x}]}` for the path `prices.owner`, which
/// plain `has_path` reports as missing. Mirrors `storage._index_field_exists`.
fn index_field_exists(doc: &Document, field: &str) -> bool {
    !get_path_values(doc, field).0.is_empty()
}

/// True if any field of `key_spec` is array-valued in `doc` — either an array
/// leaf or a dotted path descending through an array. That's the signal that
/// marks an index multikey. Mirrors `storage._doc_makes_multikey`.
fn doc_makes_multikey(doc: &Document, key_spec: &Document) -> bool {
    key_spec.keys().any(|f| index_field_values(doc, f).1)
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

    if sparse && fields.iter().any(|(f, _)| !index_field_exists(doc, f)) {
        return Ok(Vec::new());
    }

    // Per-field candidate values (see `index_field_values`), deduped on their
    // encoded bytes so a repeated array element doesn't inflate the compound
    // cartesian product below.
    let mut per_field: Vec<Vec<Bson>> = Vec::with_capacity(fields.len());
    for (f, d) in &fields {
        let (cands, _multikey) = index_field_values(doc, f);
        if cands.len() == 1 {
            per_field.push(cands);
            continue;
        }
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut uniq: Vec<Bson> = Vec::new();
        for cand in cands {
            let eb = enc_dir(&cand, *d)?;
            if seen.insert(eb) {
                uniq.push(cand);
            }
        }
        per_field.push(uniq);
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
    // Capped at MAX_COMPOUND_KEYS to avoid exponential blowup when multiple
    // fields are array-valued (real mongod rejects compound multikey on >1
    // array field; we accept it but bound the work).
    let mut combos: Vec<Vec<&Bson>> = vec![Vec::new()];
    for cand in &per_field {
        let new_size = combos.len().saturating_mul(cand.len());
        let mut next: Vec<Vec<&Bson>> = Vec::with_capacity(new_size.min(MAX_COMPOUND_KEYS + 1));
        for combo in &combos {
            for v in cand {
                if next.len() >= MAX_COMPOUND_KEYS {
                    break;
                }
                let mut c = combo.clone();
                c.push(v);
                next.push(c);
            }
            if next.len() >= MAX_COMPOUND_KEYS {
                break;
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

// There is deliberately no `index_key` (canonical one-key-per-doc) helper here:
// every caller — the uniqueness probe, the create-index pre-check, the
// duplicate finder — works off `index_key_variants`, because a doc with an
// array on an indexed path contributes several keys and mongod's rule is "no
// two docs share any generated key". Python keeps `_index_key` only for
// encoding synthetic min/max bound specs, which have no array shape.

/// Per-field values behind the entry `kb` — the `keyValue` of a dup-key error.
/// A multikey doc contributes several keys, so the conflicting one isn't
/// necessarily what `get_path` returns for the field (for a path descending
/// through an array it never is). Re-walks the candidate values to find the
/// combination that encodes to `kb`, falling back to `get_path` if none does.
/// Only called once a duplicate has been found, so the walk costs nothing on
/// the happy path. Mirrors `storage._conflict_key_value`.
fn conflict_key_value(doc: &Document, key_spec: &Document, kb: &[u8]) -> Document {
    let fields: Vec<(&String, i32)> = key_spec
        .iter()
        .map(|(k, v)| (k, direction_of(v).unwrap_or(1)))
        .collect();
    let per_field: Vec<Vec<Bson>> = fields
        .iter()
        .map(|(f, _)| index_field_values(doc, f).0)
        .collect();
    let combos = per_field.iter().try_fold(1usize, |acc, c| {
        acc.checked_mul(c.len()).filter(|n| *n <= MAX_COMPOUND_KEYS)
    });
    if combos.is_some() {
        let mut idx = vec![0usize; fields.len()];
        'outer: loop {
            let mut parts: Vec<Vec<u8>> = Vec::with_capacity(fields.len());
            for (i, (_, d)) in fields.iter().enumerate() {
                match enc_dir(&per_field[i][idx[i]], *d) {
                    Ok(b) => parts.push(b),
                    Err(_) => break,
                }
            }
            if parts.len() == fields.len() {
                let cand = if fields.len() == 1 {
                    parts.remove(0)
                } else {
                    compound_join(&parts)
                };
                if cand == kb {
                    let mut out = Document::new();
                    for (i, (f, _)) in fields.iter().enumerate() {
                        out.insert((*f).clone(), per_field[i][idx[i]].clone());
                    }
                    return out;
                }
            }
            // Odometer over the per-field candidate lists.
            for i in (0..fields.len()).rev() {
                idx[i] += 1;
                if idx[i] < per_field[i].len() {
                    continue 'outer;
                }
                idx[i] = 0;
            }
            break;
        }
    }
    let mut out = Document::new();
    for f in key_spec.keys() {
        out.insert(f.clone(), get_path(doc, f).cloned().unwrap_or(Bson::Null));
    }
    out
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

/// True if `v` is a non-empty operator document (every key starts with `$`),
/// e.g. `{$lte: 1.5}`. Mirrors Python's `all(k.startswith("$") for k in v)`.
fn is_op_doc(v: &Bson) -> bool {
    matches!(v, Bson::Document(d) if !d.is_empty() && d.keys().all(|k| k.starts_with('$')))
}

/// Does a single query constraint `(qop, qv)` guarantee the partial bound
/// `(pop, pv)`? Comparison uses `encode_value` so it follows MongoDB's
/// cross-type BSON sort order. Returns `false` for any pairing it can't prove
/// (soundness over completeness). Mirrors `storage._op_implies_bound`.
fn op_implies_bound(qop: &str, qv: &Bson, pop: &str, pv: &Bson) -> bool {
    let (a, b) = match (
        sortkey::encode_value(qv, None),
        sortkey::encode_value(pv, None),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return false,
    };
    let (le, lt, ge, gt, eq) = (a <= b, a < b, a >= b, a > b, a == b);
    match pop {
        // query upper-bounds the field; need its max <= / < pv.
        "$lte" | "$lt" => match qop {
            "$eq" | "$lte" => {
                if pop == "$lte" {
                    le
                } else {
                    lt
                }
            }
            "$lt" => le, // a < qv <= pv => a < pv => a <= pv (and a < pv for $lt)
            _ => false,
        },
        "$gte" | "$gt" => match qop {
            "$eq" | "$gte" => {
                if pop == "$gte" {
                    ge
                } else {
                    gt
                }
            }
            "$gt" => ge,
            _ => false,
        },
        "$eq" => qop == "$eq" && eq,
        _ => false,
    }
}

/// True if the query clause `qval` (a bare value or an operator dict) guarantees
/// every constraint in the partial operator dict `pbound` (e.g. `{$lte: 1.5}`).
/// Mirrors `storage._clause_implies_bounds`.
fn clause_implies_bounds(qval: &Bson, pbound: &Document) -> bool {
    let q_constraints: Vec<(&str, &Bson)> = match qval {
        Bson::Document(d) if is_op_doc(qval) => d.iter().map(|(k, v)| (k.as_str(), v)).collect(),
        _ => vec![("$eq", qval)],
    };
    for (pop, pv) in pbound.iter() {
        if !matches!(pop.as_str(), "$eq" | "$lt" | "$lte" | "$gt" | "$gte") {
            return false; // partial filter uses an operator we can't reason about
        }
        if !q_constraints
            .iter()
            .any(|(qop, qv)| op_implies_bound(qop, qv, pop, pv))
        {
            return false;
        }
    }
    true
}

/// True if every document matching `query` is guaranteed to be in a partial
/// index whose filter is `partial` — i.e. `query` is at least as restrictive as
/// `partial` on every partial-filter field. SOUNDNESS is the rule: errs to
/// `false` (skip the index, full scan) for anything it can't prove implied.
/// Supports bare-equality partial values and the `$eq`/`$lt`/`$lte`/`$gt`/`$gte`
/// range operators on both sides. Mirrors `storage._query_implies_partial`.
fn query_implies_partial(query: &Document, partial: &Document) -> bool {
    for (key, pval) in partial.iter() {
        let qval = match query.get(key) {
            Some(v) => v,
            None => return false,
        };
        if is_op_doc(pval) {
            if !clause_implies_bounds(qval, pval.as_document().unwrap()) {
                return false;
            }
        } else if is_op_doc(qval) {
            // bare-value partial, operator-form query: only an exact `$eq` of the
            // same value implies it.
            if qval.as_document().unwrap().get("$eq") != Some(pval) {
                return false;
            }
        } else if qval != pval {
            return false;
        }
    }
    true
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
fn sort_key(doc: &Document, spec: &[(String, i32)], coll: Option<&Collation>) -> Result<Vec<u8>> {
    let mut parts = Vec::with_capacity(spec.len());
    for (f, d) in spec {
        let v = get_path(doc, f).cloned().unwrap_or(Bson::Null);
        // Collation-aware sort: a strength/caseLevel collation folds string keys
        // before encoding. A collation the encoder can't reproduce (non-ASCII /
        // numericOrdering) surfaces as UnsupportedValue → command BadValue.
        parts.push(
            sortkey::encode_value_directed(&v, *d, coll)
                .map_err(|_| StorageError::UnsupportedValue)?,
        );
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

/// `(index name, packed entry key)` pairs an update must insert or remove.
type EntryOps = Vec<(String, Vec<u8>)>;

/// The per-collection write-lock registry: `(db, coll) → lock`.
type CollLocks = HashMap<(String, String), Arc<Mutex<()>>>;

/// WiredTiger-backed storage. Writes to one collection serialise on that
/// collection's lock (`coll_lock`); DDL takes the global `lock` *plus* the
/// affected collection lock(s); read-only methods run lock-free — see
/// `lock`'s invariants below.
pub struct Storage {
    /// Shared so the async-oplog drainer thread can open its own WT session from
    /// the same connection (`Arc<Connection>` derefs to `Connection`, so every
    /// `self.conn.open_session()` call site is unchanged).
    conn: Arc<Connection>,
    /// The WiredTiger home (on-disk data) directory. Kept so `create_archive`
    /// can tar the consistent file set the `backup:` cursor enumerates.
    home: String,
    /// The global lock: DDL, admin-table writes (users/roles/profile), oplog
    /// import/replay, checkpoint/archive. Plain CRUD writes do NOT take it —
    /// they serialise per collection via `coll_lock` (writes to *different*
    /// collections run in parallel; all shared counters live under the
    /// dedicated `oplog` mutex, and each write's WT rows are keyed by its own
    /// collection/seq so cross-collection writers never touch the same key).
    /// DDL takes this lock FIRST and then the affected collection lock(s), so
    /// it excludes in-flight CRUD on the namespaces it reshapes — unlike the
    /// Python server, whose per-statement WT transactions turn a DDL/CRUD
    /// overlap into a retried conflict, the Rust write path is
    /// autocommit-per-operation and needs the exclusion up front. LOCK ORDER:
    /// global before collection, collection before `oplog_prune_lock`; never
    /// acquire the global lock while holding a collection lock.
    ///
    /// Read-only methods (`find_by_id`, `find_matching_with`, the listers,
    /// planners and stats) take NO lock at all: they touch no shared Rust
    /// state (all mutable state lives in WiredTiger tables, or under the
    /// dedicated `oplog` mutex), and each call's own session gets a
    /// consistent MVCC view without blocking writers. This is safe because
    /// (a) the storage schema is a FIXED set of shared WT tables — DDL
    /// deletes rows, never drops tables, so a reader's cursor can't have its
    /// table dropped out from under it; (b) index-routed candidates are
    /// always re-verified by the exact matcher and doc fetches tolerate
    /// not-found, so a write landing between an index walk and its doc
    /// fetches can narrow but never corrupt a result (mongod's own
    /// yield-and-refresh collscan semantics). A read method that starts
    /// writing must take the appropriate write lock — `collection_uuid`'s
    /// mint path does.
    lock: Mutex<()>,
    /// Per-collection write locks: CRUD on `(db, coll)` serialises here so
    /// writes to different collections run in parallel. Entries are created
    /// on first reference and never removed — the lock identity for a
    /// namespace stays stable across drop+recreate so in-flight writers and
    /// DDL always contend on the same mutex. Mirrors `storage._coll_locks`.
    coll_locks: Mutex<CollLocks>,
    /// Serialises `prune_oplog` sweeps (the write-path opportunistic prune
    /// and the public entry point). Deliberately NOT the global lock: the
    /// opportunistic prune fires inside `emit_oplog` while the caller holds a
    /// collection lock, and taking the global lock there would invert the
    /// global→collection order (deadlock with DDL). Prune-vs-writer needs no
    /// exclusion — writers only append strictly higher seqs and never touch
    /// the old rows a prune dooms.
    oplog_prune_lock: Mutex<()>,
    /// Whether writes emit oplog entries (and the oplog tables are live). Mirrors
    /// `storage.enable_oplog`. Default `true`.
    enable_oplog: bool,
    /// Oplog recovery counters (next seq + last minted timestamp), guarded by a
    /// tiny dedicated mutex — `storage._oplog_seq_lock`. Held only for the
    /// microsecond seq/ts reservation, never across the WT cursor writes.
    /// `Arc` so the async-oplog drainer can advance `written_seq` under it.
    oplog: Arc<Mutex<OplogState>>,
    /// Change-stream tailable-wait condition, paired with the `oplog` mutex (the
    /// tail seq is `oplog.next_seq - 1`). `emit_oplog` notifies it after writing,
    /// `notify_oplog_waiters` wakes it on cursor kill; `wait_for_oplog` blocks on
    /// it. Mirrors `storage._oplog_cv` — a dedicated condition, *not* the storage
    /// `lock`, so a waiting tailable getMore can't ABBA-deadlock the write path.
    /// `Arc` so the drainer can notify it after advancing `written_seq`.
    oplog_cv: Arc<Condvar>,
    /// Retention window (seconds) and hard entry cap for `prune_oplog`. Mirrors
    /// `storage.oplog_retention_seconds` / `oplog_max_entries`.
    oplog_retention_seconds: i64,
    oplog_max_entries: usize,
    /// Per-insert discriminator for timeseries doc-table keys (see
    /// `timeseries_doc_suffix`). Wraps at 16 bits; combined with a nanosecond
    /// timestamp it keeps duplicate-`_id` rows distinct across reopens.
    ts_suffix_counter: AtomicU64,
    /// When set, `prune_oplog` archives the rows it is about to drop into a
    /// durable oplog segment in this directory first (PITR v2). Mirrors
    /// `storage.oplog_archive_dir`.
    oplog_archive_dir: Option<String>,
    /// Whether to force a WiredTiger checkpoint on close (`Drop`). Mirrors the
    /// Python `Storage._durable` flag. WT's connection close does NOT implicitly
    /// checkpoint while logging is enabled, so without a close-time checkpoint a
    /// clean shutdown leaves the log un-truncated and reopen replays the full
    /// retained log; the checkpoint bounds recovery time and truncates the log.
    /// Resolved from the environment on open (see `resolve_durable`): production
    /// (the `secantusd-rs` daemon) is durable; the Python test suite sets
    /// `SECANTUS_TEST_FAST_STORAGE=1`, which turns it off so parallel workers
    /// don't serialise on the close fsync (the `SECANTUS_FORCE_DURABLE=1` CI lane
    /// forces it back on). `durable=false` is safe only for ephemeral instances
    /// whose data dir is discarded — the journal is still on, so data is
    /// recoverable via log replay, just not checkpoint-bounded.
    durable: bool,
    /// True when opened with an `in_memory=true` WiredTiger config. WT's
    /// in-memory backend rejects `checkpoint()` (noisy `__wt_inmem_unsupported_op`
    /// log line), so the close-time checkpoint is skipped for it — mirroring
    /// Python `Storage._in_memory`.
    in_memory: bool,
    /// PROTOTYPE (opt-in via `SECANTUS_OPLOG_ASYNC=1`): when `Some`, oplog entries
    /// are NOT written inside the write's transaction. Instead the committed
    /// write's entries are minted a seq and handed to a background drainer thread
    /// that persists them off the writer's critical path — so concurrent writers
    /// stop contending on the shared oplog btrees/WAL (`tasks/rust-perf-findings.md`
    /// Finding 5). Trade: the oplog is no longer atomic with the data and a hard
    /// crash loses entries the drainer had not yet written (data stays durable;
    /// clean close flushes the drainer before the checkpoint). `None` = the
    /// synchronous, atomic default.
    async_oplog: Option<Arc<AsyncOplog>>,
}

/// Strictly-monotonic oplog bookkeeping: the next int64 seq to mint and the last
/// `Timestamp(secs, ord)` handed out. Recovered on open so post-restart mints
/// are strictly greater than anything previously emitted.
struct OplogState {
    next_seq: i64,
    last_ts_secs: i64,
    last_ts_ord: i64,
    /// Next monotonic insertion `seq` for the natural-order index (independent of
    /// the oplog `seq`; advances on every doc insert even when the oplog is off).
    next_nat_seq: i64,
    /// Oplog rows emitted since the last opportunistic prune. In-memory only
    /// (resets on open, like `storage._oplog_emit_count`); never persisted.
    emit_count: i64,
    /// Live oplog row count across all shards + the legacy table. Counted once on
    /// open, `+= n` on every `emit_oplog` / import, `-= doomed` on prune. Lets the
    /// opportunistic prune early-out (one ts read) when under the cap instead of
    /// walking the whole oplog every 1000 emits — the write-path bottleneck.
    live_count: i64,
    /// Async-oplog visibility watermark: the highest seq the background drainer
    /// has durably committed to the oplog tables. In synchronous mode this stays
    /// `next_seq - 1` (writes land inline). In async mode change-stream tailers
    /// (`wait_for_oplog`) block on this, NOT the minted `next_seq`, so a tailer
    /// never reads past what the drainer has actually written. Advanced by the
    /// drainer(s) under the `oplog` mutex, paired with `oplog_cv`.
    written_seq: i64,
    /// Async-oplog completion tracker: persisted-but-not-yet-contiguous seq ranges
    /// (`start -> end_exclusive`) reported by the parallel drainers. With a pool of
    /// shard-affine drainers, batches finish out of global seq order; a range that
    /// begins exactly at `written_seq + 1` (and any that chain onto it) is absorbed
    /// to advance `written_seq`, so the watermark still only ever covers a gapless
    /// prefix. Empty in synchronous mode. See [`advance_written_seq`].
    done_ranges: BTreeMap<i64, i64>,
}

/// Record that the seq range `[start, start + n)` is durably persisted and advance
/// `written_seq` over any newly-contiguous prefix. Returns `true` if the watermark
/// moved (the caller then notifies `oplog_cv`). Caller holds the `oplog` mutex.
fn advance_written_seq(st: &mut OplogState, start: i64, n: i64) -> bool {
    st.done_ranges.insert(start, start + n);
    let mut advanced = false;
    // Absorb the range beginning at `written_seq + 1`, then any that chain onto it.
    while let Some(&end) = st.done_ranges.get(&(st.written_seq + 1)) {
        let next = st.written_seq + 1;
        st.written_seq = end - 1;
        st.done_ranges.remove(&next);
        advanced = true;
    }
    advanced
}

// --- async oplog (prototype) ---------------------------------------------

thread_local! {
    /// Oplog entries emitted during the current write statement, buffered until
    /// the statement's transaction commits (async mode only). `with_statement_txn`
    /// drains + enqueues them on a successful commit and clears them on
    /// rollback/retry — so a rolled-back or retried write never mints a seq or
    /// enqueues an entry (no gaps in the seq space, no duplicate change events).
    /// Empty (and untouched) in the synchronous default.
    static PENDING_OPLOG: RefCell<Vec<(OplogEntry, Option<Vec<u8>>)>> =
        const { RefCell::new(Vec::new()) };

    /// Set by `with_statement_txn` for the duration of an autocommit write
    /// statement in async mode. When true, `emit_oplog_entries` buffers and lets
    /// the commit path mint + enqueue; when false (an emit NOT wrapped by
    /// `with_statement_txn`, e.g. the noop heartbeat) it buffers and drains
    /// immediately, so such entries are never stranded in `PENDING_OPLOG` and
    /// never sync-written behind the drainer's back.
    static IN_ASYNC_STMT: Cell<bool> = const { Cell::new(false) };
}

/// A contiguous run of oplog entries (one minted seq range, one shard) handed to
/// the drainer. The blobs are fully built (ts/wall already stamped) — only the
/// WiredTiger write remains.
struct DrainBatch {
    start_seq: i64,
    shard: String,
    blobs: Vec<Vec<u8>>,
    preimages: Vec<Option<Vec<u8>>>,
    /// Total bytes this batch holds (blobs + pre-images) — the amount reserved
    /// against [`Backpressure`] at enqueue and released after it is persisted.
    bytes: usize,
}

enum DrainMsg {
    Batch(DrainBatch),
    /// Flush everything buffered/queued and exit (clean shutdown).
    Shutdown,
}

/// Cap on oplog bytes queued to the drainer but not yet persisted (in the channel
/// or the reorder buffer). A sustained writer burst that outpaces the drainer
/// blocks at the enqueue point once this much is in flight — bounding memory
/// instead of growing the queue without limit. Generous: in steady state the
/// drainer keeps up and the queue stays near empty, so this only bites a
/// pathological burst or a drainer stall (e.g. a checkpoint).
const ASYNC_OPLOG_MAX_PENDING_BYTES: usize = 128 * 1024 * 1024;

/// Byte budget bounding oplog work queued to the drainer. `acquire` blocks a
/// just-committed writer while granting `n` more would exceed the cap, unless
/// nothing is outstanding (a lone oversized batch always proceeds rather than
/// deadlock); the drainer `release`s after persisting each batch. Same shape as
/// the server's `AllocBudget`. Poison-tolerant so a panicked peer can't wedge it.
struct Backpressure {
    used: Mutex<usize>,
    available: Condvar,
    cap: usize,
}

impl Backpressure {
    fn acquire(&self, n: usize) {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while *used != 0 && *used + n > self.cap {
            used = self.available.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += n;
    }

    fn release(&self, n: usize) {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        *used = used.saturating_sub(n);
        self.available.notify_all();
    }
}

/// Number of background oplog drainer threads. A single drainer's write
/// throughput (~71k entries/s here) caps sustainable async throughput below the
/// rate concurrent writers can produce (~103k/s); a pool spreads oplog writes
/// across cores (each drainer owns a disjoint set of the `OPLOG_SHARDS` btrees, so
/// no two ever write the same tree) so the drainers out-run the writers and the
/// queue stays small. Overridable via `SECANTUS_OPLOG_ASYNC_DRAINERS`.
const ASYNC_OPLOG_DRAINERS: usize = 4;

/// The drainer index that owns the shard a batch starting at `start_seq` routes to
/// (`shard % num_drainers`) — a fixed mapping, so each shard is written by exactly
/// one drainer and concurrent drainers never contend on the same btree.
fn drainer_for_batch(start_seq: i64, num_drainers: usize) -> usize {
    (start_seq.rem_euclid(OPLOG_SHARDS) as usize) % num_drainers
}

/// Handle to the background oplog drainer pool: a per-drainer channel a committed
/// write routes its entries to (by shard), the shared [`Backpressure`] budget, and
/// the drainers' join handles (joined on `Storage` drop after `Shutdown`, so a
/// clean close persists every entry before the close-time checkpoint).
struct AsyncOplog {
    txs: Vec<mpsc::Sender<DrainMsg>>,
    backpressure: Arc<Backpressure>,
    joins: Mutex<Vec<JoinHandle<()>>>,
}

/// Shared state a drainer advances so change-stream tailers can observe progress:
/// the `oplog` mutex (for `written_seq` + the completion tracker) + its condvar,
/// plus the [`Backpressure`] budget it releases as batches land.
#[derive(Clone)]
struct DrainerShared {
    conn: Arc<Connection>,
    oplog: Arc<Mutex<OplogState>>,
    oplog_cv: Arc<Condvar>,
    backpressure: Arc<Backpressure>,
}

/// Spawn the background oplog drainer pool and return the handle committed writes
/// route their entries to. The [`Backpressure`] budget is shared across all
/// drainers (one global cap on queued bytes).
fn spawn_oplog_drainer(
    conn: Arc<Connection>,
    oplog: Arc<Mutex<OplogState>>,
    oplog_cv: Arc<Condvar>,
) -> Arc<AsyncOplog> {
    // The cap is overridable via `SECANTUS_OPLOG_ASYNC_CAP_BYTES` (tuning + tests
    // that force backpressure with a tiny cap); a 0/invalid value keeps the default.
    let cap = std::env::var("SECANTUS_OPLOG_ASYNC_CAP_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(ASYNC_OPLOG_MAX_PENDING_BYTES);
    let num_drainers = std::env::var("SECANTUS_OPLOG_ASYNC_DRAINERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(ASYNC_OPLOG_DRAINERS)
        // Never more drainers than shards (a shard is owned by one drainer).
        .min(OPLOG_SHARDS as usize);
    let backpressure = Arc::new(Backpressure {
        used: Mutex::new(0),
        available: Condvar::new(),
        cap,
    });
    let shared = DrainerShared {
        conn,
        oplog,
        oplog_cv,
        backpressure: backpressure.clone(),
    };
    let mut txs = Vec::with_capacity(num_drainers);
    let mut joins = Vec::with_capacity(num_drainers);
    for _ in 0..num_drainers {
        let (tx, rx) = mpsc::channel::<DrainMsg>();
        let s = shared.clone();
        txs.push(tx);
        joins.push(thread::spawn(move || drainer_loop(s, rx)));
    }
    Arc::new(AsyncOplog {
        txs,
        backpressure,
        joins: Mutex::new(joins),
    })
}

/// Write one contiguous batch of oplog entries (+ pre-images) to its shard, in a
/// single WT transaction so a partial batch never becomes visible.
fn write_drain_batch(session: &Session, batch: &DrainBatch) -> Result<()> {
    session.begin_transaction(None)?;
    let res = (|| -> Result<()> {
        let cur = session.open_cursor(&batch.shard, None)?;
        let mut pre_cur: Option<Cursor> = None;
        for (i, blob) in batch.blobs.iter().enumerate() {
            let seq = batch.start_seq + i as i64;
            cur.reset()?;
            cur.set_key_q(seq);
            cur.set_value_u(blob);
            cur.insert()?;
            if let Some(pre) = &batch.preimages[i] {
                if pre_cur.is_none() {
                    pre_cur = Some(session.open_cursor(PREIMAGE_TABLE, None)?);
                }
                let pc = pre_cur.as_ref().unwrap();
                pc.reset()?;
                pc.set_key_q(seq);
                pc.set_value_u(pre);
                pc.insert()?;
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) => session.commit_transaction(None).map_err(Into::into),
        Err(e) => {
            let _ = session.rollback_transaction(None);
            Err(e)
        }
    }
}

/// Persist one batch and record its completion. On success: release the batch's
/// backpressure reservation and advance `written_seq` over any now-contiguous
/// prefix (waking tailers). On failure: return the batch so the caller retries it
/// (a drainer write failure is a durability signal, never silently dropped — the
/// watermark simply doesn't advance past the hole until it succeeds).
fn persist_and_record(
    session: &Session,
    shared: &DrainerShared,
    batch: DrainBatch,
) -> Option<DrainBatch> {
    let start = batch.start_seq;
    let n = batch.blobs.len() as i64;
    let bytes = batch.bytes;
    if let Err(e) = write_drain_batch(session, &batch) {
        eprintln!(
            "secantus-storage: async oplog drainer write failed at seq {start}: {e:?} (will retry)"
        );
        return Some(batch);
    }
    shared.backpressure.release(bytes);
    let advanced = {
        let mut st = shared.oplog.lock().unwrap_or_else(|e| e.into_inner());
        advance_written_seq(&mut st, start, n)
    };
    if advanced {
        shared.oplog_cv.notify_all();
    }
    None
}

/// Write a coalesced group of batches (possibly spanning several of this
/// drainer's shards) in ONE WT transaction. One cursor per distinct shard.
fn write_drain_batches(session: &Session, batches: &[DrainBatch]) -> Result<()> {
    session.begin_transaction(None)?;
    let res = (|| -> Result<()> {
        let mut curs: HashMap<&str, Cursor> = HashMap::new();
        let mut pre_cur: Option<Cursor> = None;
        for batch in batches {
            let cur = match curs.entry(batch.shard.as_str()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(session.open_cursor(&batch.shard, None)?)
                }
            };
            for (i, blob) in batch.blobs.iter().enumerate() {
                let seq = batch.start_seq + i as i64;
                cur.reset()?;
                cur.set_key_q(seq);
                cur.set_value_u(blob);
                cur.insert()?;
                if let Some(pre) = &batch.preimages[i] {
                    if pre_cur.is_none() {
                        pre_cur = Some(session.open_cursor(PREIMAGE_TABLE, None)?);
                    }
                    let pc = pre_cur.as_ref().unwrap();
                    pc.reset()?;
                    pc.set_key_q(seq);
                    pc.set_value_u(pre);
                    pc.insert()?;
                }
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) => session.commit_transaction(None).map_err(Into::into),
        Err(e) => {
            let _ = session.rollback_transaction(None);
            Err(e)
        }
    }
}

/// Persist a coalesced group in one transaction; on success release + record every
/// batch (one lock acquisition, one notify). On failure fall back to per-batch
/// writes so a single poison batch can't hold the rest hostage; per-batch failures
/// land in `retry`.
fn persist_group(
    session: &Session,
    shared: &DrainerShared,
    group: Vec<DrainBatch>,
    retry: &mut Vec<DrainBatch>,
) {
    if group.len() == 1 {
        let b = group.into_iter().next().unwrap();
        if let Some(b) = persist_and_record(session, shared, b) {
            retry.push(b);
        }
        return;
    }
    match write_drain_batches(session, &group) {
        Ok(()) => {
            let bytes: usize = group.iter().map(|b| b.bytes).sum();
            let advanced = {
                let mut st = shared.oplog.lock().unwrap_or_else(|e| e.into_inner());
                let mut adv = false;
                for b in &group {
                    adv |= advance_written_seq(&mut st, b.start_seq, b.blobs.len() as i64);
                }
                adv
            };
            shared.backpressure.release(bytes);
            if advanced {
                shared.oplog_cv.notify_all();
            }
        }
        Err(e) => {
            eprintln!(
                "secantus-storage: async oplog coalesced drain failed: {e:?} (retrying per-batch)"
            );
            for b in group {
                if let Some(b) = persist_and_record(session, shared, b) {
                    retry.push(b);
                }
            }
        }
    }
}

/// Coalescing caps: stop greedily pulling more queued batches into one drain
/// transaction past this many batches / bytes. Bounds both transaction size (WT
/// txn memory) and the visibility latency a coalesced commit adds.
const COALESCE_MAX_BATCHES: usize = 32;
const COALESCE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// One drainer thread of the pool. It owns a disjoint subset of the oplog shards
/// (routing guarantees it only receives batches for its shards), so it writes each
/// batch immediately — no reorder needed; global seq contiguity is reconstructed
/// across all drainers by `advance_written_seq`'s completion tracker. When the
/// queue is deep it coalesces up to [`COALESCE_MAX_BATCHES`] queued batches into
/// one WT transaction (fewer commits — disable with
/// `SECANTUS_OPLOG_ASYNC_COALESCE=0`). A batch whose write fails is retried on the
/// next iteration (bounded in-thread buffer). On `Shutdown` it drains everything
/// still queued before returning, so a clean close checkpoints a complete oplog.
fn drainer_loop(shared: DrainerShared, rx: mpsc::Receiver<DrainMsg>) {
    let session = match shared.conn.open_session() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("secantus-storage: async oplog drainer failed to open session: {e:?}");
            return;
        }
    };
    let coalesce = std::env::var("SECANTUS_OPLOG_ASYNC_COALESCE")
        .map(|v| v != "0")
        .unwrap_or(true);
    // Batches whose write failed, retried before processing new messages.
    let mut retry: Vec<DrainBatch> = Vec::new();
    let mut shutting_down = false;
    loop {
        // Retry any previously-failed batches first (durability before progress).
        if !retry.is_empty() {
            let mut still: Vec<DrainBatch> = Vec::new();
            for b in std::mem::take(&mut retry) {
                if let Some(b) = persist_and_record(&session, &shared, b) {
                    still.push(b);
                }
            }
            retry = still;
        }
        let msg = if shutting_down {
            rx.try_recv().ok()
        } else {
            rx.recv().ok()
        };
        match msg {
            Some(DrainMsg::Batch(b)) => {
                let mut group = vec![b];
                if coalesce {
                    let mut bytes = group[0].bytes;
                    while group.len() < COALESCE_MAX_BATCHES && bytes < COALESCE_MAX_BYTES {
                        match rx.try_recv() {
                            Ok(DrainMsg::Batch(nb)) => {
                                bytes += nb.bytes;
                                group.push(nb);
                            }
                            Ok(DrainMsg::Shutdown) => {
                                shutting_down = true;
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
                persist_group(&session, &shared, group, &mut retry);
            }
            Some(DrainMsg::Shutdown) => shutting_down = true,
            // recv error (sender dropped) or, while shutting down, queue drained.
            None => break,
        }
    }
    // Final flush: retry anything still failing (best-effort — a persistent failure
    // is a storage fault, surfaced by write_drain_batch's log).
    for b in std::mem::take(&mut retry) {
        let _ = persist_and_record(&session, &shared, b);
    }
}

/// Emit this many oplog rows between opportunistic `prune_oplog` sweeps on the
/// write path (mirrors `storage._OPLOG_PRUNE_INTERVAL`). This keeps the oplog
/// bounded from writes alone, even when the noop-heartbeat sweeper is disabled
/// (`noop_heartbeat_seconds == 0`, the default), which is the only other pruner.
const OPLOG_PRUNE_INTERVAL: i64 = 1000;

/// Upper bound on how many oldest oplog rows a single retention sweep inspects
/// when the store is under the entry cap (so an undatable/old prefix can't force a
/// whole-oplog walk on the write path). Comfortably above OPLOG_PRUNE_INTERVAL so
/// a steady-state sweep still reaches every row that aged out since the last one;
/// any excess drains on the next sweep.
const RETENTION_SCAN_BATCH: usize = 10_000;

/// Resolve the close-time `durable` flag, mirroring Python
/// `Storage.__init__`'s precedence exactly:
///   1. `SECANTUS_FORCE_DURABLE=1` — always durable, overriding everything (the
///      whole-suite real-durability CI lane).
///   2. an explicit `Some(bool)` from the caller (the Python `durable=` arg).
///   3. `None` — durable UNLESS `SECANTUS_TEST_FAST_STORAGE=1` is set (the test
///      conftest sets it so the default suite runs fast; production never sets
///      it, so the shipped daemon is fully durable).
///
/// Pure over its inputs so the precedence is unit-testable without mutating the
/// process environment (which would race parallel Rust tests).
fn resolve_durable(explicit: Option<bool>, force_durable: bool, fast_storage: bool) -> bool {
    if force_durable {
        true
    } else {
        explicit.unwrap_or(!fast_storage)
    }
}

/// Read the two durability env vars and resolve the flag for a freshly opened
/// store. `explicit` is the caller's override (`None` = env-driven default).
fn resolve_durable_from_env(explicit: Option<bool>) -> bool {
    let force = std::env::var("SECANTUS_FORCE_DURABLE").as_deref() == Ok("1");
    let fast = std::env::var("SECANTUS_TEST_FAST_STORAGE").as_deref() == Ok("1");
    resolve_durable(explicit, force, fast)
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

/// Recover the oplog counters on open: the persisted meta row clamped UP to
/// what the tables actually contain, else reconstruct from the newest oplog
/// row. Mirrors `storage._load_oplog_meta`.
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
                // The persisted counters are a *hint*, not the source of
                // truth: the meta row is written at close, not per emit, so
                // after a crash its `next_seq` / `next_nat_seq` lag the
                // tables. Trusting a stale value would re-mint an
                // already-used seq — a duplicate oplog key (lost change
                // events) or a nat-entry collision that corrupts
                // capped-collection FIFO eviction. Clamp each counter UP to
                // the table maxima; the hint only ever saves a scan, never
                // lowers us.
                return Ok(OplogState {
                    next_seq: g("next_seq")
                        .unwrap_or(1)
                        .max(scan_max_oplog_seq(session) + 1),
                    last_ts_secs: g("last_ts_secs").unwrap_or(0),
                    last_ts_ord: g("last_ts_ord").unwrap_or(0),
                    next_nat_seq: g("next_nat_seq")
                        .unwrap_or(1)
                        .max(scan_max_nat_seq(session) + 1),
                    emit_count: 0,
                    live_count: count_oplog_entries(session),
                    written_seq: 0, // set to next_seq-1 by the caller (open)
                    done_ranges: BTreeMap::new(),
                });
            }
        }
    }
    // Fallback: reconstruct from the newest oplog row. Sharded — the max seq can
    // be in any shard (`scan_max_oplog_seq` takes the max across shards + the
    // legacy table); routing is per-batch so its table isn't a function of the
    // seq — probe each table for that seq's ts.
    let last_seq = scan_max_oplog_seq(session);
    let mut last_secs = 0i64;
    let mut last_ord = 0i64;
    if last_seq > 0 {
        for tbl in oplog_all_tables() {
            let Ok(oc) = session.open_cursor(&tbl, None) else {
                continue;
            };
            oc.set_key_q(last_seq);
            if oc.search().is_ok() {
                if let Some(ts) = peek_entry_ts(&oc.get_value_u()?) {
                    last_secs = i64::from(ts.time);
                    last_ord = i64::from(ts.increment);
                }
                break;
            }
        }
    }
    Ok(OplogState {
        next_seq: last_seq + 1,
        last_ts_secs: last_secs,
        last_ts_ord: last_ord,
        next_nat_seq: scan_max_nat_seq(session) + 1,
        emit_count: 0,
        live_count: count_oplog_entries(session),
        written_seq: 0, // set to next_seq-1 by the caller (open)
        done_ranges: BTreeMap::new(),
    })
}

/// Peek the `ts` Timestamp out of a raw oplog-entry blob without materialising
/// the whole document. The prune / recovery / `startAtOperationTime` scans
/// touch every row and need only this one field; a full `decode_doc` per row
/// is the dominant cost of those walks (tasks/rust-perf-findings.md).
fn peek_entry_ts(blob: &[u8]) -> Option<bson::Timestamp> {
    let raw = bson::RawDocument::from_bytes(blob).ok()?;
    match raw.get("ts") {
        Ok(Some(bson::RawBsonRef::Timestamp(ts))) => Some(ts),
        _ => None,
    }
}

/// Largest RecordId present across the document shard tables (0 if empty) — used
/// to recover the natural-order counter on open, so minted RecordIds stay strictly
/// greater than any existing doc-table key. The RecordId is now the doc-table key
/// itself (`(db, coll, RecordId)`); the forward `NAT_TABLE` this used to scan is
/// gone. Mirrors `storage._scan_max_nat_seq`.
fn scan_max_nat_seq(session: &Session) -> i64 {
    // Doc-table keys are (db, coll, RecordId); RecordIds are global-monotonic so
    // any row's RecordId could be the max across collections/shards — scan every
    // shard and take the max.
    let mut max_seq = 0i64;
    for s in 0..DOC_SHARDS {
        let Ok(c) = session.open_cursor(&doc_shard_name(s), None) else {
            continue;
        };
        while c.next().unwrap_or(false) {
            if let Ok((_db, _coll, seq)) = c.get_key_ssq() {
                if seq > max_seq {
                    max_seq = seq;
                }
            }
        }
    }
    max_seq
}

/// What `create_archive` produced: the absolute-ish output path and its size.
/// Mirrors the `{path, sizeBytes}` Python `Storage.create_archive` returns.
pub struct ArchiveInfo {
    pub path: String,
    pub size_bytes: u64,
}

fn archive_err(ctx: &str, e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(format!("{ctx}: {e}"))
}

/// Extract a backup `.tar.gz` (from `create_archive`) into `target_dir`, which is
/// then a startable WiredTiger home. Free function (no live `Storage` needed) so
/// the restore path can rebuild a fresh directory. Mirrors Python
/// `extract_backup_archive`.
pub fn extract_backup_archive(archive_path: &str, target_dir: &str) -> Result<()> {
    std::fs::create_dir_all(target_dir).map_err(|e| archive_err("extract_backup_archive", e))?;
    let file =
        std::fs::File::open(archive_path).map_err(|e| archive_err("extract_backup_archive", e))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    archive
        .unpack(target_dir)
        .map_err(|e| archive_err("extract_backup_archive", e))?;
    Ok(())
}

/// Extract a backup `.tar.gz` into `target_dir` with the same guardrails as the
/// Python `extract_backup_archive`, returning `(abs_target, abs_archive,
/// file_count)`. Backs `secantusAdmin.restoreArchive`:
///
/// * rejects a `target_dir` that already exists, is non-empty, and
///   `allow_existing` is false;
/// * verifies the archive is a SecantusDB / WiredTiger backup (contains a
///   `WiredTiger` metadata entry) **before** extracting, so a malformed archive
///   can't pollute `target_dir`.
pub fn extract_backup_archive_ex(
    archive_path: &str,
    target_dir: &str,
    allow_existing: bool,
) -> Result<(String, String, u64)> {
    let abs_archive = std::fs::canonicalize(archive_path)
        .map_err(|_| {
            archive_err(
                "restoreArchive",
                format!("archive not found: {archive_path}"),
            )
        })?
        .to_string_lossy()
        .into_owned();

    let target = std::path::Path::new(target_dir);
    if target.exists() {
        if !target.is_dir() {
            return Err(archive_err(
                "restoreArchive",
                format!("target exists and is not a directory: {target_dir}"),
            ));
        }
        let non_empty = std::fs::read_dir(target)
            .map_err(|e| archive_err("restoreArchive", e))?
            .next()
            .is_some();
        if non_empty && !allow_existing {
            return Err(archive_err(
                "restoreArchive",
                format!(
                    "target directory is not empty (pass allowExisting=true to overlay): {target_dir}"
                ),
            ));
        }
    } else {
        std::fs::create_dir_all(target).map_err(|e| archive_err("restoreArchive", e))?;
    }

    // Pass 1: verify it's a SecantusDB backup + count entries, without touching
    // the target (mirrors Python's "check the WiredTiger metadata is present
    // before extraction so a malformed archive can't pollute target_dir").
    let mut count: u64 = 0;
    let mut has_wt = false;
    {
        let file =
            std::fs::File::open(&abs_archive).map_err(|e| archive_err("restoreArchive", e))?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in ar.entries().map_err(|e| archive_err("restoreArchive", e))? {
            let entry = entry.map_err(|e| archive_err("restoreArchive", e))?;
            let path = entry.path().map_err(|e| archive_err("restoreArchive", e))?;
            if path.as_os_str() == "WiredTiger" {
                has_wt = true;
            }
            count += 1;
        }
    }
    if !has_wt {
        return Err(archive_err(
            "restoreArchive",
            format!(
                "archive {abs_archive:?} is not a SecantusDB backup \
                 (no WiredTiger metadata file inside)"
            ),
        ));
    }

    // Pass 2: extract.
    let file = std::fs::File::open(&abs_archive).map_err(|e| archive_err("restoreArchive", e))?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    ar.unpack(target)
        .map_err(|e| archive_err("restoreArchive", e))?;

    let abs_target = std::fs::canonicalize(target)
        .map_err(|e| archive_err("restoreArchive", e))?
        .to_string_lossy()
        .into_owned();
    Ok((abs_target, abs_archive, count))
}

impl Drop for Storage {
    /// Persist the oplog meta row on teardown — the Rust analogue of
    /// `storage.close`'s `_persist_oplog_meta`. Recovery clamps against the
    /// tables anyway (`load_oplog_meta`), so a failure here costs only a
    /// tail-row scan plus a 1s clock bump on the next open — but it is still
    /// logged, never silent: in a database a close-time write error is a
    /// durability signal.
    fn drop(&mut self) {
        // Async oplog: flush the drainer pool BEFORE persisting meta / checkpointing,
        // so every committed write's oplog entry is on disk when the checkpoint
        // snapshots the tables (clean-close durability is preserved — only a hard
        // crash loses undrained entries). Signal Shutdown to every drainer and join
        // them all; each drains its queue, then exits.
        if let Some(async_h) = &self.async_oplog {
            for tx in &async_h.txs {
                let _ = tx.send(DrainMsg::Shutdown);
            }
            let handles =
                std::mem::take(&mut *async_h.joins.lock().unwrap_or_else(|e| e.into_inner()));
            for handle in handles {
                let _ = handle.join();
            }
        }
        let meta = {
            let st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.next_seq,
                st.last_ts_secs,
                st.last_ts_ord,
                st.next_nat_seq,
            )
        };
        match self.conn.open_session() {
            Ok(session) => {
                if let Err(e) = self.persist_oplog_meta(&session, meta) {
                    eprintln!("secantus-storage: failed to persist oplog meta during close: {e:?}");
                }
                // Close-time checkpoint (durable mode) — the Rust analogue of
                // Python `Storage.close`'s final checkpoint. WiredTiger's
                // connection close does NOT implicitly checkpoint while logging
                // is enabled, so without this a clean shutdown leaves the log
                // un-truncated and the next open replays the full retained log.
                // Run it AFTER persisting the oplog-meta row so that row is
                // included in the snapshot. Skipped in fast/test mode
                // (`durable=false`) — the journal is still on, so data stays
                // recoverable via log replay, just not checkpoint-bounded — and
                // for in-memory backends, which reject checkpoint(). By this
                // point the caller (`RunningServer::stop`) has drained every
                // connection thread, so this runs single-threaded with the data
                // dir intact (no repeat of the beta.48 checkpoint-vs-teardown
                // race). A checkpoint failure is logged, never silent: in a
                // database a close-time write error is a durability signal.
                if self.durable && !self.in_memory {
                    if let Err(e) = session.checkpoint(None) {
                        eprintln!("secantus-storage: final checkpoint failed during close: {e:?}");
                    }
                }
            }
            Err(e) => {
                eprintln!("secantus-storage: failed to open session during close: {e:?}");
            }
        }
    }
}

impl Storage {
    /// Open (creating if needed) an on-disk database at `home` with the default
    /// SecantusDB WiredTiger config, bootstrapping the table schema.
    pub fn open(home: &str) -> Result<Storage> {
        Self::open_with_config(home, DEFAULT_CONFIG)
    }

    /// Open with an explicit WiredTiger config (e.g. add `in_memory=true` for an
    /// ephemeral database). The close-time `durable` flag is resolved from the
    /// environment (see [`resolve_durable`]); use [`Storage::open_with_config_durable`]
    /// to override it explicitly.
    pub fn open_with_config(home: &str, config: &str) -> Result<Storage> {
        Self::open_with_config_durable(home, config, None)
    }

    /// Like [`Storage::open_with_config`], but with an explicit close-time
    /// `durable` override. `None` = env-driven default (`SECANTUS_FORCE_DURABLE` /
    /// `SECANTUS_TEST_FAST_STORAGE`); `Some(true)`/`Some(false)` force it, mirroring
    /// the Python `Storage(durable=...)` argument. `SECANTUS_FORCE_DURABLE=1` still
    /// wins over an explicit `Some(false)`, matching Python's precedence.
    pub fn open_with_config_durable(
        home: &str,
        config: &str,
        durable: Option<bool>,
    ) -> Result<Storage> {
        let durable = resolve_durable_from_env(durable);
        let in_memory = config.contains("in_memory=true");
        let conn = Arc::new(Connection::open(home, config)?);
        let mut state = {
            let boot = conn.open_session()?;
            for (name, fmt) in BOOTSTRAP {
                boot.create(name, fmt)?;
            }
            // The oplog is sharded across OPLOG_SHARDS btrees to spread append
            // contention (see OPLOG_SHARDS); the legacy single table stays
            // so a pre-shard store's entries remain readable. Config honours
            // SECANTUS_OPLOG_NONLOGGED (see `oplog_table_cfg`).
            let opcfg = oplog_table_cfg();
            boot.create(OPLOG_TABLE, &opcfg)?;
            boot.create(PREIMAGE_TABLE, &opcfg)?;
            for s in 0..OPLOG_SHARDS {
                boot.create(&oplog_shard_name(s), &opcfg)?;
            }
            // The documents table is sharded per collection (see DOC_SHARDS); the
            // legacy single table above stays in BOOTSTRAP so a pre-shard store's
            // rows remain reachable for the one-time migration below.
            for s in 0..DOC_SHARDS {
                boot.create(&doc_shard_name(s), DOC_TABLE_CFG)?;
            }
            // Fail fast on a store whose doc shards predate the RecordId keying
            // change (`SSu` on disk vs the `SSq` this build needs): no in-place
            // upgrade, refuse to open rather than mis-read. WT `create` above
            // preserves an existing table's key_format, so the on-disk schema is
            // intact for this check. (Runs before `migrate_legacy_docs` — that
            // path is the pre-*shard* case, which this check does not touch.)
            reject_pre_recordid_doc_format(&boot)?;
            // Same fail-fast for the index-ENTRY format (step 2). Runs after the
            // doc-table check so the more fundamental mismatch is reported first.
            reject_legacy_index_entry_format(&boot)?;
            // One-time: fold a pre-shard store's legacy documents rows into the
            // per-collection shards (no-op for a born-sharded store).
            migrate_legacy_docs(&boot)?;
            // Recover the oplog seq / timestamp counters from the meta row, or
            // reconstruct them by scanning the oplog table.
            load_oplog_meta(&boot)?
        };
        // Cluster-time mints are not persisted per call (see
        // `current_cluster_time`), so the recovered (last_ts_secs, last_ts_ord)
        // can lag mints issued right before a crash. Bump one full second past
        // everything recovered: any unpersisted mint carried the wall-clock
        // second it was issued in, which is <= max(recovered, now) — so +1s is
        // strictly greater than all of them. Costs at most a 1s forward jump of
        // the (already logical) cluster clock per restart; never applied to a
        // virgin store. Mirrors `storage.__init__`'s recovery bump.
        if state.last_ts_secs > 0 {
            state.last_ts_secs = state.last_ts_secs.max(now_secs()) + 1;
            state.last_ts_ord = 0;
        }
        // The async drainer starts caught up to the recovered tail: nothing is
        // in flight yet, so every seq < next_seq is already durably on disk.
        state.written_seq = state.next_seq - 1;
        let oplog = Arc::new(Mutex::new(state));
        let oplog_cv = Arc::new(Condvar::new());
        // PROTOTYPE opt-in: spawn the background drainer when SECANTUS_OPLOG_ASYNC
        // is set. The synchronous, atomic path is the default (`None`).
        let async_oplog = if std::env::var_os("SECANTUS_OPLOG_ASYNC").is_some() {
            Some(spawn_oplog_drainer(
                conn.clone(),
                oplog.clone(),
                oplog_cv.clone(),
            ))
        } else {
            None
        };
        Ok(Storage {
            conn,
            home: home.to_string(),
            lock: Mutex::new(()),
            coll_locks: Mutex::new(HashMap::new()),
            oplog_prune_lock: Mutex::new(()),
            enable_oplog: true,
            oplog,
            oplog_cv,
            oplog_retention_seconds: 3600,
            oplog_max_entries: 100_000,
            ts_suffix_counter: AtomicU64::new(0),
            oplog_archive_dir: None,
            durable,
            in_memory,
            async_oplog,
        })
    }

    /// Turn oplog emission on/off (mirrors `SecantusDBServer(enable_oplog=...)`).
    /// Off means writes skip the oplog tables entirely.
    pub fn set_enable_oplog(&mut self, on: bool) {
        self.enable_oplog = on;
    }

    /// Enable PITR v2 oplog archiving: `prune_oplog` writes the rows it is about
    /// to drop into a durable segment in `dir` first. `None` disables it. Mirrors
    /// `Storage(oplog_archive_dir=...)`.
    pub fn set_oplog_archive_dir(&mut self, dir: Option<String>) {
        self.oplog_archive_dir = dir;
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

    // --- locking (per-collection write locks + write-conflict retry) ---

    /// The per-collection write lock for `(db, coll)`, created on first
    /// reference and never removed (stable identity across drop+recreate, so
    /// in-flight writers and DDL always contend on the same mutex). CRUD on a
    /// collection serialises through this; CRUD on other collections runs in
    /// parallel. DDL takes the global `lock` first, then this. Mirrors
    /// `storage._coll_lock`.
    fn coll_lock(&self, db: &str, coll: &str) -> Arc<Mutex<()>> {
        let mut reg = self.coll_locks.lock().unwrap_or_else(|e| e.into_inner());
        reg.entry((db.to_string(), coll.to_string()))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Whether the calling thread is inside a user (multi-document)
    /// transaction (its session installed by `with_user_transaction`).
    fn in_user_txn(&self) -> bool {
        !ACTIVE_TXN_SESSION.with(|c| c.get()).is_null()
    }

    /// Retry `f` on `WriteConflict` until it goes through — UNBOUNDED,
    /// matching mongod's `writeConflictRetry`: a client of mongod never sees
    /// `WriteConflict` for a plain write outside a multi-document
    /// transaction, so neither should ours. A warning is logged every few
    /// seconds of continuous retrying so a pathological livelock is visible.
    /// Inside a user transaction the conflict is NOT retried: it surfaces
    /// immediately so the command layer can abort the transaction with
    /// mongod's statement-time `WriteConflict`. Mirrors
    /// `storage._retry_write_conflicts`.
    ///
    /// Retrying a *partially applied* statement is safe here even though the
    /// write path is autocommit-per-operation (no batch transaction to roll
    /// back): a conflict can only come from a user transaction's uncommitted
    /// write on the same key, and every write path touches the contended
    /// doc-table row BEFORE (insert/replace/delete) or in lockstep with its
    /// derived rows — whose keys embed the doc's own id_key, so a derived-row
    /// conflict implies the doc-row op would have conflicted first. The
    /// re-run therefore starts from either a clean slate or a prefix whose
    /// re-application is idempotent (entry inserts overwrite; entry removes
    /// tolerate not-found; the update path's entry diff recomputes from the
    /// current doc).
    fn retry_write_conflicts<T>(&self, op: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        const DELAY: std::time::Duration = std::time::Duration::from_millis(5);
        const DELAY_MAX: std::time::Duration = std::time::Duration::from_millis(20);
        const LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
        let mut delay = DELAY;
        let mut started: Option<std::time::Instant> = None;
        let mut last_log = std::time::Instant::now();
        loop {
            match f() {
                Err(StorageError::WriteConflict) if !self.in_user_txn() => {
                    let now = std::time::Instant::now();
                    match started {
                        None => {
                            started = Some(now);
                            last_log = now;
                        }
                        Some(s) => {
                            if now.duration_since(last_log) >= LOG_EVERY {
                                last_log = now;
                                eprintln!(
                                    "secantus-storage: {op} retrying on write conflicts for {:.1}s",
                                    now.duration_since(s).as_secs_f64()
                                );
                            }
                        }
                    }
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(DELAY_MAX);
                }
                r => return r,
            }
        }
    }

    /// Run one write statement inside its own WT transaction (snapshot
    /// isolation): commit on success, roll back on error — unless the thread
    /// is inside a user transaction, in which case the statement joins it and
    /// the user-transaction machinery owns commit/abort.
    ///
    /// This is what makes a statement's read-modify-write safe under
    /// concurrency: without it each cursor operation autocommits separately,
    /// so an update could read a document in one implicit transaction and
    /// write it in another — a competitor committing in between is silently
    /// overwritten with a value computed from the stale read (a lost update;
    /// pinned by tests/concurrent_writes.rs). Inside one snapshot transaction
    /// WiredTiger detects the write to a since-committed document and fails
    /// it — WT_ROLLBACK at the op, or bare EINVAL at commit when a competitor
    /// marked us rollback-only after our last op (WT's documented errno for
    /// committing a rollback-required transaction; the reason text goes only
    /// to the event handler) — and both map to the retriable `WriteConflict`.
    /// It also makes the statement atomic: doc row, index entries,
    /// natural-order rows and oplog rows commit or vanish together, so a
    /// crash mid-statement can no longer leave a dangling index entry.
    /// Mirrors `storage._batch_transaction` + `_commit_batch_transaction`.
    fn with_statement_txn<T>(
        &self,
        session: &OpSession<'_>,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if matches!(session, OpSession::Txn(_)) {
            return f();
        }
        // Async oplog: mark this statement so `emit_oplog_entries` buffers its
        // entries instead of writing them in-transaction. The buffer is minted +
        // handed to the drainer only on a successful commit (below), and cleared
        // on rollback/retry — so a rolled-back write neither mints a seq (no gap)
        // nor enqueues an entry (no duplicate change event).
        let async_mode = self.async_oplog.is_some();
        if async_mode {
            IN_ASYNC_STMT.with(|f| f.set(true));
            PENDING_OPLOG.with(|p| p.borrow_mut().clear());
        }
        session.begin_transaction(None)?;
        match f() {
            Ok(v) => {
                if let Err(e) = session.commit_transaction(None) {
                    let _ = session.rollback_transaction(None);
                    if async_mode {
                        IN_ASYNC_STMT.with(|f| f.set(false));
                        PENDING_OPLOG.with(|p| p.borrow_mut().clear());
                    }
                    const EINVAL: i32 = 22;
                    if e.is_rollback() || e.code == EINVAL {
                        return Err(StorageError::WriteConflict);
                    }
                    return Err(e.into());
                }
                if async_mode {
                    // Committed: mint seqs for the buffered entries and hand them
                    // to the drainer (which persists them + wakes tailers).
                    IN_ASYNC_STMT.with(|f| f.set(false));
                    self.drain_pending_oplog();
                } else {
                    // Sync: wake tailable change-stream waiters only now that the
                    // rows are visible to their fresh-session reads (the
                    // in-statement notify fires before commit, which a waiter's
                    // re-read can miss).
                    self.notify_oplog_waiters();
                }
                Ok(v)
            }
            Err(e) => {
                let _ = session.rollback_transaction(None);
                if async_mode {
                    IN_ASYNC_STMT.with(|f| f.set(false));
                    PENDING_OPLOG.with(|p| p.borrow_mut().clear());
                }
                Err(e)
            }
        }
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
        let mut st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        let start = st.next_seq;
        st.next_seq += n as i64;
        let ts: Vec<bson::Timestamp> = (0..n).map(|_| Self::mint_ts(&mut st)).collect();
        (start, ts)
    }

    /// Build a CRUD oplog entry as raw BSON — `{op, ns, (ui,) o, o2}` in mongod
    /// field order — splicing the pre-encoded `o` / `o2` document bytes so the
    /// document body is never re-serialized (the oplog-encode hot path: for an
    /// insert `o` is the full stored blob, for a replacement update it's the
    /// already-computed new blob). `ts` + `wall` are appended later by
    /// [`Self::emit_oplog_entries`]. Byte-equivalent to the owned-`Document` form.
    fn oplog_entry_crud(
        op: &str,
        ns: &str,
        ui: Option<&[u8]>,
        o: &[u8],
        o2: &[u8],
    ) -> Result<bson::RawDocumentBuf> {
        let mut buf = bson::RawDocumentBuf::new();
        buf.append("op", bson::RawBsonRef::String(op).to_raw_bson());
        buf.append("ns", bson::RawBsonRef::String(ns).to_raw_bson());
        if let Some(u) = ui {
            buf.append(
                "ui",
                bson::RawBsonRef::Binary(bson::raw::RawBinaryRef {
                    subtype: BinarySubtype::Uuid,
                    bytes: u,
                })
                .to_raw_bson(),
            );
        }
        let o_raw =
            bson::RawDocument::from_bytes(o).map_err(|e| StorageError::Bson(e.to_string()))?;
        let o2_raw =
            bson::RawDocument::from_bytes(o2).map_err(|e| StorageError::Bson(e.to_string()))?;
        buf.append("o", bson::RawBsonRef::Document(o_raw).to_raw_bson());
        buf.append("o2", bson::RawBsonRef::Document(o2_raw).to_raw_bson());
        Ok(buf)
    }

    /// Owned-`Document` oplog entries (the rare DDL / noop / findAndModify paths).
    /// Wraps each as [`OplogEntry::Doc`] and defers to [`Self::emit_oplog_entries`].
    fn emit_oplog(
        &self,
        session: &Session,
        entries: Vec<Document>,
        pre_images: Vec<Option<Vec<u8>>>,
    ) -> Result<i64> {
        self.emit_oplog_entries(
            session,
            entries.into_iter().map(OplogEntry::Doc).collect(),
            pre_images,
        )
    }

    /// Append `entries` to the oplog table, stamping each with its minted `ts`
    /// and a `wall` time, and return the highest seq written (0 if disabled or
    /// empty). `pre_images` is parallel to `entries`; a `Some(bytes)` element is
    /// stored under the matching seq in the pre-image table. Callers hold their
    /// write lock (the collection lock for CRUD, the global lock for DDL /
    /// admin paths); concurrent emitters are safe regardless — each writes only
    /// its own freshly-minted seqs. Mirrors `storage._emit_oplog`.
    fn emit_oplog_entries(
        &self,
        session: &Session,
        entries: Vec<OplogEntry>,
        pre_images: Vec<Option<Vec<u8>>>,
    ) -> Result<i64> {
        if !self.enable_oplog || entries.is_empty() {
            return Ok(0);
        }
        debug_assert_eq!(pre_images.len(), entries.len());
        // Async oplog (prototype): inside an autocommit write statement, buffer
        // the entries instead of writing them in this transaction. They are minted
        // a seq and handed to the drainer by `with_statement_txn` after the data
        // transaction commits (`drain_pending_oplog`). In async mode ALL emission
        // must go through the drainer: a synchronous WT write here would mint a
        // seq the drainer never sees, leaving a permanent hole in its contiguous
        // `written_seq` run (a stall). So an emit outside a wrapped statement
        // (IN_ASYNC_STMT false — e.g. the noop heartbeat) buffers and drains
        // immediately, treating itself as its own committed unit.
        if self.async_oplog.is_some() {
            PENDING_OPLOG.with(|p| {
                let mut p = p.borrow_mut();
                for pair in entries.into_iter().zip(pre_images) {
                    p.push(pair);
                }
            });
            if !IN_ASYNC_STMT.with(|f| f.get()) {
                self.drain_pending_oplog();
            }
            return Ok(0);
        }
        let n = entries.len() as i64;
        let (start, ts) = self.mint_seq_and_ts(entries.len());
        // Route the WHOLE batch to one shard (`start % OPLOG_SHARDS`) so concurrent
        // writers — minting different start seqs — spread across N append points
        // instead of contending on one table's rightmost page (the scaling fix),
        // while each batch stays a contiguous sequential append to a single tree
        // (the locality that per-entry scatter destroyed). One cursor for the run.
        let cur = session.open_cursor(&oplog_shard_for_batch(start), None)?;
        let mut pre_cur: Option<Cursor> = None;
        let wall_millis = now_millis();
        let wall = Bson::DateTime(bson::DateTime::from_millis(wall_millis));
        let mut last = 0i64;
        for (i, entry) in entries.into_iter().enumerate() {
            let seq = start + i as i64;
            // Stamp `ts` + `wall` (appended last, matching the historical field
            // order `[…, o2, ts, wall]`) and produce the entry's BSON bytes. The
            // `Raw` path splices the pre-encoded body straight through; the `Doc`
            // path re-encodes the owned document as before.
            let blob = match entry {
                OplogEntry::Doc(mut d) => {
                    d.insert("ts", Bson::Timestamp(ts[i]));
                    d.insert("wall", wall.clone());
                    encode_doc(&d)?
                }
                OplogEntry::Raw(mut buf) => {
                    buf.append("ts", bson::RawBsonRef::Timestamp(ts[i]).to_raw_bson());
                    buf.append(
                        "wall",
                        bson::RawBsonRef::DateTime(bson::DateTime::from_millis(wall_millis))
                            .to_raw_bson(),
                    );
                    buf.into_bytes()
                }
            };
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
        // Wake any tailable change-stream getMore blocked in `wait_for_oplog`:
        // the rows are committed (autocommit per insert) and the tail (next_seq)
        // has advanced, so a woken waiter's producer re-read sees the new events.
        // Also bump the opportunistic-prune counter under the same lock.
        let do_prune = {
            let mut g = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
            self.oplog_cv.notify_all();
            g.emit_count += n;
            g.live_count += n;
            if g.emit_count >= OPLOG_PRUNE_INTERVAL {
                g.emit_count = 0;
                true
            } else {
                false
            }
        };
        // Opportunistically bound the oplog from write volume alone (mirrors
        // `storage._emit_oplog`'s every-1000-emits prune). Sweep exclusivity
        // comes from `oplog_prune_lock` inside `prune_oplog_inner` — the
        // caller holds only its collection lock here, and the prune must not
        // touch the global lock (lock order). Best-effort: the write already
        // committed, so a prune failure must not fail the write — the next
        // sweep retries.
        if do_prune {
            if let Err(e) = self.prune_oplog_inner(None) {
                debug_assert!(false, "opportunistic prune_oplog failed: {e:?}");
            }
        }
        Ok(last)
    }

    /// Async oplog (prototype): mint seqs + timestamps for this thread's buffered
    /// entries (from a write that just committed) and hand them to the drainer as
    /// one contiguous batch. Minting here — AFTER the data commit — is what keeps
    /// the seq space gapless: a rolled-back/retried write cleared its buffer
    /// before reaching this point, so it never minted. `wait_for_oplog` waits on
    /// the drainer's `written_seq`, so a tailer never reads past what is on disk.
    fn drain_pending_oplog(&self) {
        let Some(async_h) = self.async_oplog.clone() else {
            return;
        };
        let pending: Vec<(OplogEntry, Option<Vec<u8>>)> =
            PENDING_OPLOG.with(|p| std::mem::take(&mut *p.borrow_mut()));
        if pending.is_empty() {
            return;
        }
        let n = pending.len();
        let (start, ts) = self.mint_seq_and_ts(n);
        let wall_millis = now_millis();
        let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(n);
        let mut preimages: Vec<Option<Vec<u8>>> = Vec::with_capacity(n);
        for (i, (entry, pre)) in pending.into_iter().enumerate() {
            let blob = match entry {
                OplogEntry::Doc(mut d) => {
                    d.insert("ts", Bson::Timestamp(ts[i]));
                    d.insert(
                        "wall",
                        Bson::DateTime(bson::DateTime::from_millis(wall_millis)),
                    );
                    // A failed oplog encode is a bug (our own docs); keep the seq
                    // filled with a valid empty document so the drainer's
                    // contiguity (and thus every later entry) is never stalled.
                    encode_doc(&d).unwrap_or_else(|e| {
                        eprintln!("secantus-storage: async oplog encode failed: {e:?}");
                        vec![5, 0, 0, 0, 0]
                    })
                }
                OplogEntry::Raw(mut buf) => {
                    buf.append("ts", bson::RawBsonRef::Timestamp(ts[i]).to_raw_bson());
                    buf.append(
                        "wall",
                        bson::RawBsonRef::DateTime(bson::DateTime::from_millis(wall_millis))
                            .to_raw_bson(),
                    );
                    buf.into_bytes()
                }
            };
            blobs.push(blob);
            preimages.push(pre);
        }
        {
            let mut g = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
            g.live_count += n as i64;
            g.emit_count += n as i64;
        }
        let bytes: usize = blobs.iter().map(Vec::len).sum::<usize>()
            + preimages
                .iter()
                .filter_map(|p| p.as_ref().map(Vec::len))
                .sum::<usize>();
        let batch = DrainBatch {
            start_seq: start,
            shard: oplog_shard_for_batch(start),
            blobs,
            preimages,
            bytes,
        };
        // Backpressure: reserve this batch's bytes before enqueuing. If the
        // drainers have fallen behind and too much is already in flight, this blocks
        // the committing writer until they catch up — bounding memory rather than
        // letting the queue grow without limit. Reserve BEFORE send so the in-flight
        // budget always covers what's queued.
        async_h.backpressure.acquire(bytes);
        // Route to the drainer that owns this batch's shard (fixed mapping — the
        // shard is written by exactly one drainer, so drainers never collide).
        let d = drainer_for_batch(start, async_h.txs.len());
        if async_h.txs[d].send(DrainMsg::Batch(batch)).is_err() {
            // Drainer gone (shutdown race): release the reservation we just took
            // so the budget doesn't leak, and report the dropped entries loudly.
            async_h.backpressure.release(bytes);
            eprintln!(
                "secantus-storage: async oplog drainer gone; {n} committed entries not persisted"
            );
        }
    }

    /// Block until the oplog tail seq exceeds `after_seq` (a new entry landed),
    /// or `timeout_ms` elapses, or a waiter is woken (`notify_oplog_waiters`).
    /// Returns the current tail seq. One bounded wait — a spurious wake returns
    /// early and the caller re-drains (matching the Python tailable getMore's
    /// single `_oplog_cv.wait_for`). The tail / wait check share the `oplog`
    /// mutex, so there's no lost-wakeup; the wait releases that mutex so writers
    /// can still mint seqs. Used by the change-stream tailable getMore path.
    pub fn wait_for_oplog(&self, after_seq: i64, timeout_ms: u64) -> i64 {
        // In async mode the visible tail is what the drainer has DURABLY written
        // (`written_seq`), not the minted `next_seq` — a tailer must never read
        // past a not-yet-persisted entry. In sync mode `written_seq` tracks
        // `next_seq - 1`, so the same expression is exact there too.
        let async_mode = self.async_oplog.is_some();
        let tail = |st: &OplogState| {
            if async_mode {
                st.written_seq
            } else {
                st.next_seq - 1
            }
        };
        let guard = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        if tail(&guard) > after_seq {
            return tail(&guard);
        }
        let (guard, _timed_out) = self
            .oplog_cv
            .wait_timeout(guard, Duration::from_millis(timeout_ms))
            .unwrap();
        tail(&guard)
    }

    /// Async oplog: block until the drainer has durably persisted every entry
    /// minted so far (`written_seq == next_seq - 1`). No-op in synchronous mode
    /// (where the two are always equal). Gives a caller read-after-write oplog
    /// visibility — a consistency checkpoint, a test, or a drain before backup.
    pub fn flush_oplog(&self) {
        if self.async_oplog.is_none() {
            return;
        }
        let mut st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        while st.written_seq < st.next_seq - 1 {
            let (g, _timed_out) = self
                .oplog_cv
                .wait_timeout(st, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner());
            st = g;
        }
    }

    /// Wake every `wait_for_oplog` waiter without advancing the oplog (e.g. on
    /// `killCursors`, so a blocked tailable getMore returns promptly to observe
    /// its cursor's invalidation). Mirrors `storage._oplog_cv.notify_all()`.
    pub fn notify_oplog_waiters(&self) {
        let _g = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        self.oplog_cv.notify_all();
    }

    /// A strictly-monotonic `Timestamp` advancing the cluster clock (used for
    /// `hello`'s `lastWrite` / the `aggregate` reply's `operationTime`).
    /// Deliberately does NOT persist the oplog meta row: this runs on every
    /// `hello` reply under the replica-set persona (driver heartbeats) and on
    /// change-stream high-water-mark minting, so a per-call meta write is a
    /// single-row WT hotspot every concurrent writer contends on. Restart
    /// monotonicity is guaranteed structurally instead: recovery bumps the
    /// clock one second past everything it can see (see `open_with_config`),
    /// which covers any mint that was never persisted; the meta row itself is
    /// written at close (`Drop`). Mirrors `storage.current_cluster_time`.
    pub fn current_cluster_time(&self) -> Result<bson::Timestamp> {
        // The seq counter can't be demoted to a bare `AtomicI64`:
        // `wait_for_oplog` pairs `next_seq` with `oplog_cv`, and the condvar's
        // lost-wakeup guarantee requires the counter to be read under the same
        // mutex as the wait. Minting under the dedicated oplog mutex alone is
        // enough — no WT access, so the global lock adds nothing here.
        let mut st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Self::mint_ts(&mut st))
    }

    /// The last minted cluster time WITHOUT advancing the clock. Reply gossip
    /// (`$clusterTime` / `operationTime` on every reply) observes cluster time;
    /// only writes and the explicit `current_cluster_time` advance it. A virgin
    /// store mints once so the gossiped value is never `Timestamp(0, 0)`.
    /// Mirrors `storage.peek_cluster_time`.
    pub fn peek_cluster_time(&self) -> Result<bson::Timestamp> {
        {
            let st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
            if st.last_ts_secs != 0 {
                return Ok(bson::Timestamp {
                    time: st.last_ts_secs as u32,
                    increment: st.last_ts_ord as u32,
                });
            }
        }
        self.current_cluster_time()
    }

    /// Persist the recovery meta row (`next_seq` / `last_ts_*`). Best-effort
    /// optimisation — `load_oplog_meta` reconstructs from the oplog table if the
    /// row is stale or missing. Mirrors `storage._persist_oplog_meta`.
    fn persist_oplog_meta(&self, session: &Session, meta: (i64, i64, i64, i64)) -> Result<()> {
        let (next_seq, last_ts_secs, last_ts_ord, next_nat_seq) = meta;
        let mut d = Document::new();
        d.insert("next_seq", next_seq);
        d.insert("last_ts_secs", last_ts_secs);
        d.insert("last_ts_ord", last_ts_ord);
        d.insert("next_nat_seq", next_nat_seq);
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
        // No global lock: a fresh session's WiredTiger MVCC snapshot gives a
        // consistent read without blocking writers. This is the hot tailable
        // change-stream getMore path, so serialising it against every write
        // would needlessly throttle throughput.
        let session = self.conn.open_session()?;
        read_oplog_shards(&session, start_seq, limit)
    }

    /// Whether `(db, coll)` is the synthetic `local.oplog.rs` view.
    fn is_oplog_rs(&self, db: &str, coll: &str) -> bool {
        self.enable_oplog && db == "local" && coll == "oplog.rs"
    }

    /// Read path for the synthetic `local.oplog.rs` view: every persisted oplog
    /// entry, filtered by `filter` and ordered by `$natural` (the oplog's only
    /// meaningful order — entries are scanned in seq == insertion == ts order,
    /// so `$natural: 1` is the identity and `$natural: -1` reverses). A
    /// non-`$natural` sort falls through to the generic compound-key post-sort.
    /// skip / limit / projection are the caller's job. Mirrors
    /// `storage._find_oplog_rs`.
    fn find_oplog_rs(
        &self,
        filter: &Document,
        sort: Option<&Document>,
        coll_opt: Option<&Collation>,
        vars: &Document,
    ) -> Result<Vec<Vec<u8>>> {
        let rows = self.read_oplog(0, usize::MAX)?;
        let mut out: Vec<(Document, Vec<u8>)> = Vec::with_capacity(rows.len());
        for (_seq, blob) in rows {
            let d = decode_doc(&blob)?;
            if filter.is_empty()
                || query_matches(&d, filter, vars, coll_opt)
                    .map_err(|_| StorageError::QueryUnsupported)?
            {
                out.push((d, blob));
            }
        }
        if let Some(s) = sort {
            if let Some(nat) = s.get("$natural") {
                let dir = nat
                    .as_i32()
                    .or_else(|| nat.as_i64().map(|v| v as i32))
                    .or_else(|| nat.as_f64().map(|v| v as i32))
                    .unwrap_or(1);
                if dir < 0 {
                    out.reverse();
                }
            } else if let Some(spec) = multi_sort_spec(sort) {
                let mut keyed: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(out.len());
                for (d, blob) in out {
                    keyed.push((sort_key(&d, &spec, coll_opt)?, blob));
                }
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                return Ok(keyed.into_iter().map(|(_, b)| b).collect());
            }
        }
        Ok(out.into_iter().map(|(_, b)| b).collect())
    }

    /// The smallest seq currently present (0 if empty) — the retention floor a
    /// resume token must stay at or above. Mirrors `storage.oplog_floor_seq`.
    pub fn oplog_floor_seq(&self) -> Result<i64> {
        // Lock-free cross-thread read on a fresh MVCC session (see `read_oplog`).
        // Sharded: the global floor is the smallest first-key across all shards
        // (+ the legacy table). Each shard's `next()` from the start lands on its
        // minimum.
        let session = self.conn.open_session()?;
        let mut floor: Option<i64> = None;
        for s in 0..OPLOG_SHARDS {
            let cur = session.open_cursor(&oplog_shard_name(s), None)?;
            if cur.next()? {
                let seq = cur.get_key_q()?;
                floor = Some(floor.map_or(seq, |f: i64| f.min(seq)));
            }
        }
        let cur = session.open_cursor(OPLOG_TABLE, None)?;
        if cur.next()? {
            let seq = cur.get_key_q()?;
            floor = Some(floor.map_or(seq, |f: i64| f.min(seq)));
        }
        Ok(floor.unwrap_or(0))
    }

    /// The highest seq emitted (`next_seq - 1`), 0 if none. Mirrors
    /// `storage.oplog_tail_seq`.
    pub fn oplog_tail_seq(&self) -> i64 {
        // The tail counter lives under the dedicated oplog mutex; the global lock
        // adds nothing here.
        self.oplog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_seq
            - 1
    }

    /// Force a WiredTiger checkpoint (durable flush of the latest snapshot). Used
    /// by oplog replay to make the restored database durable before the target
    /// `Storage` is dropped.
    pub fn checkpoint(&self) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        session.checkpoint(None)?;
        Ok(())
    }

    /// Force a checkpoint, then tar the consistent WiredTiger file set (enumerated
    /// by WiredTiger's `backup:` cursor) into `output_path` as a gzip stream, with
    /// an advisory `pitr-manifest.json` describing the oplog range it can recover
    /// to. Mirrors Python `Storage.create_archive`. The on-disk + oplog formats
    /// are identical across the two servers, so the Python restore tooling reads
    /// this archive (and vice versa).
    pub fn create_archive(&self, output_path: &str) -> Result<ArchiveInfo> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        // Durable, consistent snapshot first; the backup cursor enumerates the
        // files that make it up and WiredTiger holds them stable for the cursor's
        // lifetime, so we tar them all before it drops.
        session.checkpoint(None)?;
        let manifest = self.pitr_manifest()?;
        let cursor = session.open_cursor("backup:", None)?;
        let file =
            std::fs::File::create(output_path).map_err(|e| archive_err("create_archive", e))?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        while cursor.next()? {
            let rel = cursor.get_key_s()?;
            let full = std::path::Path::new(&self.home).join(&rel);
            builder
                .append_path_with_name(&full, &rel)
                .map_err(|e| archive_err("create_archive", e))?;
        }
        let data =
            serde_json::to_vec(&manifest).map_err(|e| archive_err("create_archive manifest", e))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, PITR_MANIFEST_NAME, &data[..])
            .map_err(|e| archive_err("create_archive", e))?;
        builder
            .into_inner()
            .map_err(|e| archive_err("create_archive", e))?
            .finish()
            .map_err(|e| archive_err("create_archive", e))?;
        let size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
        Ok(ArchiveInfo {
            path: output_path.to_string(),
            size_bytes: size,
        })
    }

    /// Take a PITR v2 base snapshot into `archive_dir`, named by its oplog head
    /// seq (`base-<head>.tar.gz`) so the restore path can order and select
    /// snapshots. Thin wrapper over `create_archive`. Pair with
    /// `set_oplog_archive_dir(archive_dir)` and call periodically (no background
    /// scheduler). Mirrors `storage.archive_base_snapshot`.
    pub fn archive_base_snapshot(&self, archive_dir: &str) -> Result<ArchiveInfo> {
        let head = self.oplog_tail_seq();
        std::fs::create_dir_all(archive_dir)
            .map_err(|e| archive_err("archive_base_snapshot", e))?;
        let out = std::path::Path::new(archive_dir).join(pitr_archive::base_name(head));
        self.create_archive(&out.to_string_lossy())
    }

    /// Read the doomed oplog rows (and pre-images) and write them to a durable
    /// segment in `archive_dir` before `prune_oplog` deletes them. Best-effort
    /// reads — a row that vanished concurrently is skipped.
    fn archive_doomed_oplog(
        &self,
        session: &Session,
        archive_dir: &str,
        doomed_sorted: &[i64],
    ) -> Result<()> {
        // Sharded: routing is per-batch, so a doomed seq's table isn't a function
        // of the seq — probe every table (shards + legacy) for the row. Lazy
        // cursor cache; `search()` (a read) reports not-found honestly regardless
        // of overwrite mode.
        let tables = oplog_all_tables();
        let mut op_curs: Vec<Option<Cursor>> = tables.iter().map(|_| None).collect();
        let pre_cur = session.open_cursor(PREIMAGE_TABLE, None)?;
        let mut rows: Vec<(i64, Document, Option<Document>)> = Vec::new();
        for &seq in doomed_sorted {
            let mut blob: Option<Vec<u8>> = None;
            for (t, name) in tables.iter().enumerate() {
                if op_curs[t].is_none() {
                    op_curs[t] = Some(session.open_cursor(name, None)?);
                }
                let op_cur = op_curs[t].as_ref().unwrap();
                op_cur.reset()?;
                op_cur.set_key_q(seq);
                if op_cur.search().is_ok() {
                    blob = Some(op_cur.get_value_u()?);
                    break;
                }
            }
            let Some(blob) = blob else { continue };
            if blob.is_empty() {
                continue;
            }
            let entry = decode_doc(&blob)?;
            pre_cur.reset()?;
            pre_cur.set_key_q(seq);
            let pre = match pre_cur.search() {
                Ok(()) => {
                    let pb = pre_cur.get_value_u()?;
                    if pb.is_empty() {
                        None
                    } else {
                        Some(decode_doc(&pb)?)
                    }
                }
                Err(_) => None,
            };
            rows.push((seq, entry, pre));
        }
        pitr_archive::write_segment(archive_dir, &rows)?;
        Ok(())
    }

    /// Build the advisory PITR manifest embedded in a backup archive: the oplog
    /// seq range and timestamps it can recover to, plus whether the oplog still
    /// reaches genesis. Mirrors Python `Storage._pitr_manifest` (a subset; wall
    /// times land with the v2 base-selection work). Called under `self.lock`; the
    /// oplog reads it uses are lock-free.
    fn pitr_manifest(&self) -> Result<serde_json::Value> {
        let floor = self.oplog_floor_seq()?;
        let head = self.oplog_tail_seq();
        let row_of = |seq: i64| -> Option<Document> {
            if seq <= 0 {
                return None;
            }
            let rows = self.read_oplog(seq, 1).ok()?;
            let (_s, blob) = rows.into_iter().next()?;
            decode_doc(&blob).ok()
        };
        let ts_of = |row: &Option<Document>| -> Option<[i64; 2]> {
            match row.as_ref()?.get("ts") {
                Some(Bson::Timestamp(ts)) => Some([i64::from(ts.time), i64::from(ts.increment)]),
                _ => None,
            }
        };
        let wall_of = |row: &Option<Document>| -> Option<String> {
            match row.as_ref()?.get("wall") {
                Some(Bson::DateTime(dt)) => dt.try_to_rfc3339_string().ok(),
                _ => None,
            }
        };
        let floor_row = row_of(floor);
        let head_row = row_of(head);
        Ok(serde_json::json!({
            "secantusPitrManifest": 1,
            "oplogEnabled": self.enable_oplog,
            "oplogFloorSeq": floor,
            "oplogHeadSeq": head,
            "genesisIntact": floor == 1,
            "oplogFloorTs": ts_of(&floor_row),
            "oplogHeadTs": ts_of(&head_row),
            "oplogFloorWall": wall_of(&floor_row),
            "oplogHeadWall": wall_of(&head_row),
        }))
    }

    /// Merge `opts` into the collection's options blob (creating the collection
    /// if needed) — e.g. `{changeStreamPreAndPostImages: {enabled: true}}`.
    /// Mirrors `storage.set_collection_options`.
    pub fn set_collection_options(&self, db: &str, coll: &str, opts: &Document) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;
        let mut current = coll_options(&session, db, coll)?.unwrap_or_default();
        for (k, v) in opts {
            current.insert(k.clone(), v.clone());
        }
        write_coll_options(&session, db, coll, &current)
    }

    /// `collMod`: merge `opts` into the collection's stored options (like
    /// `set_collection_options`) AND emit a DDL `op: "c"` `collMod` oplog entry,
    /// so a `showExpandedEvents` change stream surfaces a `modify` event. Used by
    /// the `collMod` command (plain `set_collection_options` stays oplog-silent
    /// for internal option writes such as `create`'s). Mirrors mongod's collMod
    /// oplog (`o = {collMod: <coll>, <changed fields>}`).
    pub fn coll_mod(&self, db: &str, coll: &str, opts: &Document) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        ensure_collection(&session, db, coll)?;
        let mut current = coll_options(&session, db, coll)?.unwrap_or_default();
        for (k, v) in opts {
            current.insert(k.clone(), v.clone());
        }
        write_coll_options(&session, db, coll, &current)?;
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let mut o = Document::new();
            o.insert("collMod", coll);
            for (k, v) in opts {
                o.insert(k.clone(), v.clone());
            }
            let mut entry = Document::new();
            entry.insert("op", "c");
            entry.insert("ns", format!("{db}.$cmd"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
        Ok(())
    }

    /// The collection's 16-byte UUID (minting + persisting one on first use).
    /// Mirrors `storage.collection_uuid`.
    pub fn collection_uuid(&self, db: &str, coll: &str) -> Result<Vec<u8>> {
        // Fast path: already minted — plain lock-free read (this runs on
        // change-stream open, so it shouldn't queue behind writers). Mirrors
        // `storage._collection_uuid`'s fast path.
        let session = self.conn.open_session()?;
        if let Some(opts) = coll_options(&session, db, coll)? {
            if let Some(Bson::Binary(b)) = opts.get("uuid") {
                if b.bytes.len() == 16 {
                    return Ok(b.bytes.clone());
                }
            }
        }
        // Mint path: take the SAME collection write lock every CRUD writer's
        // lazy mint runs under, so two racers can't mint different UUIDs for
        // one namespace; the free helper re-reads inside the lock and mints
        // only if still absent.
        let lock = self.coll_lock(db, coll);
        let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
        ensure_collection(&session, db, coll)?;
        collection_uuid(&session, db, coll)
    }

    /// Whether `changeStreamPreAndPostImages: {enabled: true}` is set on the
    /// collection — change streams need it for `fullDocument: required` /
    /// `whenAvailable` (post-image) and `fullDocumentBeforeChange`.
    pub fn pre_post_images_enabled(&self, db: &str, coll: &str) -> Result<bool> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        pre_post_images_enabled(&session, db, coll)
    }

    /// The pre-image document bytes stored for oplog `seq`, or `None`. Fresh
    /// session for cross-thread visibility. Mirrors `storage.read_preimage`.
    pub fn read_preimage(&self, seq: i64) -> Result<Option<Vec<u8>>> {
        // Lock-free cross-thread read on a fresh MVCC session (see `read_oplog`).
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

    /// Write **verbatim** oplog rows `(seq, entry)` (carrying their original seq /
    /// ts / wall) plus any pre-images into the oplog tables, and advance the seq +
    /// cluster clock past them — so a restored store continues a pre-existing
    /// timeline and a change stream there can resume from a token minted before
    /// the restore point. Unlike `emit_oplog` this does NOT mint new seqs (the
    /// rows keep their identity, so resume tokens stay valid) and is not gated on
    /// `enable_oplog` (it's an explicit import). Returns the highest seq written.
    /// Mirrors Python `Storage.import_oplog_segment`.
    pub fn import_oplog_segment(
        &self,
        rows: &[(i64, Document)],
        pre_images: &std::collections::HashMap<i64, Vec<u8>>,
    ) -> Result<i64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        // Restored rows keep their original seq identity. Route the whole import
        // to one shard (by its first seq) — any shard is fine since the merge-read
        // and all-table point-ops find rows regardless of placement; a single
        // contiguous append keeps the restore fast (per-seq routing would scatter).
        let cur = session.open_cursor(&oplog_shard_for_batch(rows[0].0), None)?;
        let mut pre_cur: Option<Cursor> = None;
        let mut max_seq = 0i64;
        let mut best = (0i64, 0i64);
        for (seq, entry) in rows {
            let blob = encode_doc(entry)?;
            cur.reset()?;
            cur.set_key_q(*seq);
            cur.set_value_u(&blob);
            cur.insert()?;
            if let Some(pre) = pre_images.get(seq) {
                if pre_cur.is_none() {
                    pre_cur = Some(session.open_cursor(PREIMAGE_TABLE, None)?);
                }
                let pc = pre_cur.as_ref().unwrap();
                pc.reset()?;
                pc.set_key_q(*seq);
                pc.set_value_u(pre);
                pc.insert()?;
            }
            if *seq > max_seq {
                max_seq = *seq;
            }
            if let Some(Bson::Timestamp(ts)) = entry.get("ts") {
                let cand = (i64::from(ts.time), i64::from(ts.increment));
                if cand > best {
                    best = cand;
                }
            }
        }
        {
            let mut st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
            if max_seq + 1 > st.next_seq {
                st.next_seq = max_seq + 1;
            }
            if best > (st.last_ts_secs, st.last_ts_ord) {
                st.last_ts_secs = best.0;
                st.last_ts_ord = best.1;
            }
            // Imported rows are live oplog entries too — keep the prune's count honest.
            st.live_count += rows.len() as i64;
            self.oplog_cv.notify_all();
        }
        Ok(max_seq)
    }

    /// Drop oplog rows older than the retention window (`ts.time < now -
    /// retention`) and, if more than `oplog_max_entries` remain, the oldest
    /// surplus; paired pre-images go too. `now` is injected seconds (defaults to
    /// the wall clock). Returns the number of rows pruned. No background sweeper —
    /// the caller drives it. Mirrors `storage.prune_oplog` / `_prune_oplog_locked`.
    pub fn prune_oplog(&self, now: Option<i64>) -> Result<usize> {
        self.prune_oplog_inner(now)
    }

    /// One prune sweep, exclusive with other sweeps via `oplog_prune_lock` —
    /// NOT the global lock: the opportunistic write-path caller
    /// (`emit_oplog`) holds a collection lock, and global-after-collection
    /// would invert the lock order (deadlock with DDL). Pruner-vs-writer
    /// needs no exclusion — writers only append strictly higher seqs and
    /// never touch the old rows doomed here; the reads that could observe a
    /// half-pruned range (`read_oplog`, resume) tolerate missing rows.
    fn prune_oplog_inner(&self, now: Option<i64>) -> Result<usize> {
        let _p = self
            .oplog_prune_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let when = now.unwrap_or_else(now_secs);
        let cutoff = when - self.oplog_retention_seconds;

        // Phase 1 (lock-free): identify the doomed rows WITHOUT scanning the whole
        // oplog — that full scan, every OPLOG_PRUNE_INTERVAL emits, was 77% of the
        // single-writer write-path CPU (profile: scratchpad/profile_insert.sh). The
        // live-count lets us size the sweep: `excess` is how many oldest rows must
        // drop to get back under the entry cap; retention dooms a seq-ordered
        // prefix on top of that. A fresh MVCC session reads consistently without
        // blocking writers; prune is best-effort, so a slightly stale count/view is
        // fine — writers only append *higher* seqs, never touch the old rows here.
        let tables = oplog_all_tables();
        let live_count = self.oplog.lock().unwrap().live_count;
        let excess = (live_count - self.oplog_max_entries as i64).max(0) as usize;

        // Cheap early-out: under the cap, the only reason to prune is retention,
        // which dooms a seq-ordered prefix — so if the OLDEST live row is still
        // in-window, nothing is doomed. One bounded merge read of a single row
        // replaces the whole-oplog walk in the common steady state.
        if excess == 0 {
            match read_oplog_shards_tagged(&session, 0, 1)?.first() {
                None => return Ok(0),
                Some((_, _, blob)) => {
                    if matches!(peek_entry_ts(blob), Some(ts) if i64::from(ts.time) >= cutoff) {
                        return Ok(0);
                    }
                    // oldest is out-of-window (or undatable) — fall through to walk.
                }
            }
        }

        // Bounded walk of the oldest rows only: a row is doomed if it's within the
        // cap excess (i < excess) OR past retention (ts < cutoff). Both doom a
        // seq-ordered prefix, so stop at the first row that is neither. The read is
        // bounded by max(excess, RETENTION_SCAN_BATCH); any retention rows beyond
        // that drain on later sweeps. The tagged merge carries each row's source
        // table so phase 2 deletes from exactly that table. An undatable row ends
        // the retention prefix (keep what we can't date) but is still cap-doomed if
        // i < excess.
        let limit = excess.max(RETENTION_SCAN_BATCH);
        let rows = read_oplog_shards_tagged(&session, 0, limit)?;
        let mut doomed: Vec<(i64, usize)> = Vec::new();
        for (i, (seq, tbl, blob)) in rows.iter().enumerate() {
            let past_retention =
                matches!(peek_entry_ts(blob), Some(ts) if i64::from(ts.time) < cutoff);
            if i < excess || past_retention {
                doomed.push((*seq, *tbl));
            } else {
                break;
            }
        }
        if doomed.is_empty() {
            return Ok(0);
        }
        let doomed_seqs: Vec<i64> = doomed.iter().map(|(s, _)| *s).collect();

        // PITR v2: archive the soon-to-be-dropped rows to a durable segment
        // *before* deleting them, so recovery can still reach a time before the
        // new oplog floor.
        if let Some(archive_dir) = self.oplog_archive_dir.clone() {
            self.archive_doomed_oplog(&session, &archive_dir, &doomed_seqs)?;
        }

        // Phase 2: the deletes. Sweep exclusivity is already held
        // (`oplog_prune_lock`, taken at the top); no other lock is needed —
        // concurrent writers only ever append higher seqs. Each doomed row is
        // removed from its exact source table (the phase-1 tag). Pre-images stay
        // in one table.
        let mut del_curs: Vec<Option<Cursor>> = tables.iter().map(|_| None).collect();
        let pre_del = session.open_cursor(PREIMAGE_TABLE, None)?;
        for (seq, tbl) in &doomed {
            if del_curs[*tbl].is_none() {
                del_curs[*tbl] = Some(session.open_cursor(&tables[*tbl], None)?);
            }
            let op_del = del_curs[*tbl].as_ref().unwrap();
            op_del.reset()?;
            op_del.set_key_q(*seq);
            match op_del.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
            pre_del.reset()?;
            pre_del.set_key_q(*seq);
            match pre_del.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Keep the live-count honest for the next sweep's sizing.
        self.oplog.lock().unwrap().live_count -= doomed.len() as i64;
        Ok(doomed.len())
    }

    /// Append one `{op: "n", ns: "", o: {msg: "periodic noop"}}` heartbeat and
    /// return its seq — keeps a quiet collection's resume token advancing with
    /// cluster time. Mirrors `storage.emit_noop_heartbeat`.
    pub fn emit_noop_heartbeat(&self) -> Result<i64> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
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
        // Lock-free cross-thread read on a fresh MVCC session (see `read_oplog`).
        // Sharded: ts is monotone in the *global* seq order, so the shard merge
        // yields entries in ts order — the first one at/after `target` is the
        // answer.
        let session = self.conn.open_session()?;
        for (seq, blob) in read_oplog_shards(&session, 0, usize::MAX)? {
            if let Some(e) = peek_entry_ts(&blob) {
                if e.time > ts.time || (e.time == ts.time && e.increment >= ts.increment) {
                    return Ok(seq);
                }
            }
        }
        Ok(self
            .oplog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_seq)
    }

    // -- user (multi-document) transactions --------------------------------
    //
    // A user transaction owns a dedicated WT session (see `UserTransactionHandle`).
    // `with_user_transaction` installs that session into a thread-local for the
    // duration of one statement, so every CRUD path's `op_session()` transparently
    // routes its cursors through the WT transaction — read-your-own-writes and the
    // pinned snapshot fall out for free, exactly as `storage.py`'s
    // `use_user_transaction` swaps the thread-local WT session. The command layer
    // serializes statements per transaction; these primitives assume no two
    // threads install the same handle concurrently.

    /// The session a transaction-participating CRUD statement should use: the
    /// active user-transaction session when one is installed on this thread, else
    /// a fresh autocommit session. The deliberately cross-thread oplog reads
    /// (`read_oplog` / `read_preimage` / `oplog_floor_seq` / `find_seq_for_ts`)
    /// and the cluster-time / meta paths bypass this and stay on a fresh session.
    fn op_session(&self) -> Result<OpSession<'_>> {
        let p = ACTIVE_TXN_SESSION.with(|c| c.get());
        if p.is_null() {
            Ok(OpSession::Fresh(self.conn.open_session()?))
        } else {
            // SAFETY: `with_user_transaction` installs this pointer to a `Session`
            // it owns, for the strict duration of the closure running on THIS
            // thread, and clears it before returning — so the referent outlives
            // every `op_session` call the statement makes. The per-transaction
            // mutex in the command layer guarantees no concurrent access.
            Ok(OpSession::Txn(unsafe { &*p }))
        }
    }

    /// Open a dedicated WT session for a new multi-document transaction. The WT
    /// `begin_transaction` is deferred to the first `with_user_transaction`.
    pub fn begin_user_transaction(&self) -> Result<UserTransactionHandle> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        Ok(UserTransactionHandle {
            session: Some(session),
            began: false,
        })
    }

    /// Run `f` with `handle`'s session installed as this thread's transaction
    /// session (beginning the WT transaction lazily on first entry). Every
    /// storage call `f` makes routes through that session, so it executes inside
    /// the WT transaction. The previous thread-local state is restored on return
    /// — including on panic — so nested / re-entrant use is safe.
    pub fn with_user_transaction<T>(
        &self,
        handle: &mut UserTransactionHandle,
        f: impl FnOnce() -> T,
    ) -> Result<T> {
        let session = handle
            .session
            .as_ref()
            .ok_or_else(|| StorageError::Internal("transaction already closed".into()))?;
        if !handle.began {
            session.begin_transaction(None)?;
            handle.began = true;
        }
        struct Restore(*const Session);
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTIVE_TXN_SESSION.with(|c| c.set(self.0));
            }
        }
        let _restore = Restore(ACTIVE_TXN_SESSION.with(|c| c.get()));
        ACTIVE_TXN_SESSION.with(|c| c.set(session as *const Session));
        Ok(f())
    }

    /// Commit the transaction's WT session, then **close** it (releasing the WT
    /// resource). Idempotent: a handle whose session is already closed is a
    /// no-op. A commit failure still closes the session (its `Drop` rolls back
    /// the uncommitted transaction).
    pub fn commit_user_transaction(&self, handle: &mut UserTransactionHandle) -> Result<()> {
        if let Some(session) = handle.session.take() {
            let began = handle.began;
            handle.began = false;
            if began {
                if let Err(e) = session.commit_transaction(None) {
                    // A concurrent transaction can mark this one rollback-only
                    // after its last statement ran; WiredTiger then fails the
                    // commit call itself with bare EINVAL (its documented
                    // errno for committing a rollback-required transaction —
                    // the "requires rollback: conflict between concurrent
                    // operations" text goes only to the event handler). Our
                    // commit config is a fixed literal, so EINVAL here has
                    // exactly one cause: surface it as the retriable
                    // `WriteConflict`, exactly like a statement-time
                    // WT_ROLLBACK (port of the Python server's
                    // `_commit_batch_transaction` mapping). Every other code
                    // — WT_PANIC included — stays loud: a commit failure that
                    // isn't a conflict is a durability signal.
                    const EINVAL: i32 = 22;
                    if e.is_rollback() || e.code == EINVAL {
                        return Err(StorageError::WriteConflict);
                    }
                    return Err(e.into());
                }
                // Wake tailable change-stream waiters: the transaction's
                // oplog rows became visible at this commit, not at emit.
                self.notify_oplog_waiters();
            }
            // `session` drops here → the dedicated WT session is closed.
        }
        Ok(())
    }

    /// Roll back the transaction's WT session, then **close** it. Idempotent;
    /// best-effort rollback (closing the session also rolls back).
    pub fn rollback_user_transaction(&self, handle: &mut UserTransactionHandle) -> Result<()> {
        if let Some(session) = handle.session.take() {
            let began = handle.began;
            handle.began = false;
            if began {
                let _ = session.rollback_transaction(None);
            }
            // `session` drops here → the dedicated WT session is closed.
        }
        Ok(())
    }

    /// Insert one BSON-encoded document. Assigns an `ObjectId` `_id` if absent.
    /// Returns the document's `id_key`. A duplicate `_id` yields
    /// `StorageError::DuplicateId`.
    /// Whether `(db, coll)` is a timeseries collection (its stored options carry
    /// a `timeseries` sub-document). Mirrors `storage._is_timeseries`.
    fn is_timeseries(&self, session: &Session, db: &str, coll: &str) -> Result<bool> {
        Ok(coll_options(session, db, coll)?
            .map(|o| o.contains_key("timeseries"))
            .unwrap_or(false))
    }

    /// A doc-table key discriminator for a timeseries collection. Timeseries
    /// collections don't enforce `_id` uniqueness, but the doc table is keyed by
    /// `id_key(_id)`, so equal `_id`s would collide. Appending this suffix keeps
    /// duplicates distinct while preserving `_id` grouping (the sortkey encoding
    /// is prefix-free). A nanosecond timestamp survives reopens; a 16-bit counter
    /// disambiguates same-nanosecond inserts. Reads decode + filter by content,
    /// so the suffix is invisible above storage — but the `_id` point-lookup fast
    /// path must be skipped (it reconstructs the UNsuffixed key). Mirrors
    /// `storage._timeseries_doc_suffix`.
    fn timeseries_doc_suffix(&self) -> Vec<u8> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let counter = (self.ts_suffix_counter.fetch_add(1, Ordering::Relaxed) & 0xFFFF) as u16;
        let mut out = Vec::with_capacity(10);
        out.extend_from_slice(&nanos.to_be_bytes());
        out.extend_from_slice(&counter.to_be_bytes());
        out
    }

    pub fn insert_one(&self, db: &str, coll: &str, doc_bytes: &[u8]) -> Result<Vec<u8>> {
        self.retry_write_conflicts("insert_one", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut doc = decode_doc(doc_bytes)?;
            let assigned_id = !doc.contains_key("_id");
            if assigned_id {
                doc.insert("_id", Bson::ObjectId(ObjectId::new()));
            }
            let id = doc.get("_id").expect("_id present").clone();
            let mut key = id_key(&id)?;
            // Raw-write fast path: reuse the caller's BSON verbatim when already in
            // canonical storage form (`_id` first, none assigned) — see `insert`.
            let id_first = doc.keys().next().map(String::as_str) == Some("_id");
            let reencoded;
            let blob: &[u8] = if !assigned_id && id_first {
                doc_bytes
            } else {
                reencoded = encode_doc(&doc)?;
                &reencoded
            };
            if blob.len() > MAX_BSON_OBJECT_SIZE {
                return Err(StorageError::DocumentTooLarge(blob.len()));
            }

            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                ensure_collection(&session, db, coll)?;
                // Timeseries: suffix the doc-table key so duplicate `_id`s coexist.
                if self.is_timeseries(&session, db, coll)? {
                    key.extend_from_slice(&self.timeseries_doc_suffix());
                }
                // Reject unique-index violations before writing anything.
                let descs = self.index_descs(&session, db, coll)?;
                if let Some(c) = self.unique_conflict(&session, db, coll, &doc, &descs, None)? {
                    return Err(StorageError::DuplicateKey(Box::new(c)));
                }
                // Mint the RecordId + write the `_id` index (id_key -> RecordId);
                // this overwrite=false insert is where a duplicate `_id` is caught.
                let recordid = self.write_nat_entry(&session, db, coll, &key)?;
                // Doc table keyed by the (unique) RecordId.
                let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                cur.set_key_ssq(db, coll, recordid);
                cur.set_value_u(&frame_doc_value(&key, blob));
                cur.insert()?;
                // Maintain secondary indexes: write this doc's entries (still carrying
                // id_key as the fetch pointer in step 1; IXSCAN resolves id_key ->
                // RecordId -> doc), and lazily flag any index this doc makes multikey.
                self.write_index_entries(&session, db, coll, &doc, &descs, recordid)?;
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
                    // `doc` is no longer needed after this — move it in rather than clone.
                    entry.insert("o", Bson::Document(doc));
                    entry.insert("o2", Bson::Document(o2));
                    self.emit_oplog(&session, vec![entry], vec![None])?;
                }
                Ok(key)
            })
        })
    }

    /// Batch insert. Each element of `docs` is a BSON-encoded document; a
    /// missing `_id` is assigned an `ObjectId`. Returns `(inserted, errors)`
    /// where `errors` are mongod-shaped write-error docs (`{index, code,
    /// errmsg, keyPattern?, keyValue?}`) for duplicate-`_id` / unique-index
    /// violations. `ordered` stops at the first error (else continues). All
    /// successful inserts share one batched oplog emit. Capped collections
    /// evict oldest non-fresh docs to stay within `size`/`max` bounds. Mirrors
    /// `storage.insert`. (Geo-index validation is not yet enforced by the Rust
    /// engine — see backlog.)
    pub fn insert(
        &self,
        db: &str,
        coll: &str,
        docs: Vec<Vec<u8>>,
        ordered: bool,
    ) -> Result<(usize, Vec<Document>)> {
        self.retry_write_conflicts("insert", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                ensure_collection(&session, db, coll)?;
                let descs = self.index_descs(&session, db, coll)?;
                let ns = format!("{db}.{coll}");
                let oplog_on = self.enable_oplog;
                let timeseries = self.is_timeseries(&session, db, coll)?;
                let ui = if oplog_on {
                    Some(collection_uuid(&session, db, coll)?)
                } else {
                    None
                };
                let mut inserted = 0usize;
                let mut errors: Vec<Document> = Vec::new();
                let mut oplog_entries: Vec<OplogEntry> = Vec::new();
                let mut fresh_id_keys: HashSet<Vec<u8>> = HashSet::new();
                let doc_cur =
                    session.open_cursor(&doc_table_for(db, coll), Some("overwrite=false"))?;
                for (index, doc_bytes) in docs.iter().enumerate() {
                    let mut doc = decode_doc(doc_bytes)?;
                    let assigned_id = !doc.contains_key("_id");
                    if assigned_id {
                        doc.insert("_id", Bson::ObjectId(ObjectId::new()));
                    }
                    let id = doc.get("_id").expect("_id present").clone();
                    let mut key = id_key(&id)?;
                    // Timeseries: suffix the doc-table key so duplicate `_id`s coexist.
                    if timeseries {
                        key.extend_from_slice(&self.timeseries_doc_suffix());
                    }
                    // Unique-index pre-check (collect a write-error rather than abort).
                    if let Some(c) = self.unique_conflict(&session, db, coll, &doc, &descs, None)? {
                        let ns = format!("{db}.{coll}");
                        let mut e = Document::new();
                        e.insert("index", index as i32);
                        e.insert("code", 11000i32);
                        e.insert("errmsg", format_dup_key_errmsg(&ns, &c.index, &c.key_value));
                        e.insert("keyPattern", Bson::Document(c.key_pattern));
                        e.insert("keyValue", Bson::Document(c.key_value));
                        errors.push(e);
                        if ordered {
                            break;
                        }
                        continue;
                    }
                    // Raw-write fast path: reuse the caller's BSON verbatim when it
                    // is already in mongod's canonical storage form (`_id` first, and
                    // no `_id` assigned here). encode_doc(&doc) would reproduce these
                    // exact bytes, so re-encoding is dead work; mongod likewise stores
                    // the document as the client sent it. Falls back to encode_doc when
                    // an ObjectId was assigned or `_id` is not the leading field (the
                    // reorder case encode_doc handles).
                    let id_first = doc.keys().next().map(String::as_str) == Some("_id");
                    let reencoded;
                    let blob: &[u8] = if !assigned_id && id_first {
                        doc_bytes
                    } else {
                        reencoded = encode_doc(&doc)?;
                        &reencoded
                    };
                    if blob.len() > MAX_BSON_OBJECT_SIZE {
                        errors.push(too_large_write_error(index, blob.len()));
                        if ordered {
                            break;
                        }
                        continue;
                    }
                    // Mint the RecordId + write the `_id` index (id_key -> RecordId);
                    // a duplicate `_id` is caught here now (not by the doc-table
                    // insert, which is keyed by the unique RecordId). WT_DUPLICATE_KEY
                    // does not abort the transaction, so unordered inserts continue.
                    let recordid = match self.write_nat_entry(&session, db, coll, &key) {
                        Ok(r) => r,
                        Err(StorageError::DuplicateId) => {
                            let ns = format!("{db}.{coll}");
                            let mut key_value = Document::new();
                            key_value.insert("_id", id.clone());
                            let mut key_pattern = Document::new();
                            key_pattern.insert("_id", 1i32);
                            let mut ed = Document::new();
                            ed.insert("index", index as i32);
                            ed.insert("code", 11000i32);
                            ed.insert(
                                "errmsg",
                                format_dup_key_errmsg(&ns, ID_INDEX_NAME, &key_value),
                            );
                            ed.insert("keyPattern", Bson::Document(key_pattern));
                            ed.insert("keyValue", Bson::Document(key_value));
                            errors.push(ed);
                            if ordered {
                                break;
                            }
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    // Doc table keyed by the (unique) RecordId.
                    doc_cur.reset()?;
                    doc_cur.set_key_ssq(db, coll, recordid);
                    doc_cur.set_value_u(&frame_doc_value(&key, blob));
                    doc_cur.insert()?;
                    self.write_index_entries(&session, db, coll, &doc, &descs, recordid)?;
                    self.maybe_mark_multikey(&session, db, coll, &doc, &descs)?;
                    fresh_id_keys.insert(key.clone());
                    inserted += 1;
                    if oplog_on {
                        // `o` splices the stored doc bytes (`blob`) verbatim — no
                        // re-encode of the document body (the oplog hot-path win);
                        // `o2` is the tiny `{_id}`. `doc` is not used afterward.
                        let o2 = encode_id_doc(&id)?;
                        oplog_entries.push(OplogEntry::Raw(Self::oplog_entry_crud(
                            "i",
                            &ns,
                            Some(ui.as_ref().unwrap()),
                            blob,
                            &o2,
                        )?));
                    }
                }
                // Capped-collection eviction: drop oldest non-fresh docs until within
                // the collection's `size` / `max` bounds. Inserts have no pre-image, so
                // the per-insert pre-image slots are all None; eviction appends its own.
                let mut pre_images: Vec<Option<Vec<u8>>> = vec![None; oplog_entries.len()];
                if inserted > 0 {
                    self.enforce_capped_bounds(
                        &session,
                        db,
                        coll,
                        &fresh_id_keys,
                        &descs,
                        oplog_on,
                        &ns,
                        ui.as_deref(),
                        &mut oplog_entries,
                        &mut pre_images,
                    )?;
                }
                if oplog_on && !oplog_entries.is_empty() {
                    self.emit_oplog_entries(&session, oplog_entries, pre_images)?;
                }
                Ok((inserted, errors))
            })
        })
    }

    /// Fetch a document by `_id`. Returns its BSON bytes, or `None`.
    pub fn find_by_id(&self, db: &str, coll: &str, id: &Bson) -> Result<Option<Vec<u8>>> {
        // Lock-free read (see the `lock` field's invariants).
        let key = id_key(id)?;
        let session = self.op_session()?;
        // Resolve `_id` -> RecordId via the `_id` index, then fetch the doc row.
        let Some(recordid) = self.doc_recordid(&session, db, coll, &key)? else {
            return Ok(None);
        };
        let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        cur.set_key_ssq(db, coll, recordid);
        match cur.search() {
            Ok(()) => {
                let value = cur.get_value_u()?;
                let (_idk, blob) = unframe_doc_value(&value)?;
                Ok(Some(blob.to_vec()))
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// All documents of a collection in natural (insertion / RecordId) order, as
    /// BSON bytes.
    pub fn scan_collection(&self, db: &str, coll: &str) -> Result<Vec<Vec<u8>>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.op_session()?;
        Ok(self
            .scan_docs(&session, db, coll)?
            .into_iter()
            .map(|(_rid, _idk, blob)| blob)
            .collect())
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
        self.retry_write_conflicts("replace_by_id", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let key = id_key(id)?;
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                // Existence check — resolve the doc's RecordId via the `_id` index,
                // then read its row so we can retract its entries.
                let Some(recordid) = self.doc_recordid(&session, db, coll, &key)? else {
                    return Ok(false);
                };
                let probe = session.open_cursor(&doc_table_for(db, coll), None)?;
                probe.set_key_ssq(db, coll, recordid);
                let old_blob = match probe.search() {
                    Ok(()) => {
                        let value = probe.get_value_u()?;
                        let (_idk, blob) = unframe_doc_value(&value)?;
                        blob.to_vec()
                    }
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
                if let Some(c) =
                    self.unique_conflict(&session, db, coll, &doc, &descs, Some(recordid))?
                {
                    return Err(StorageError::DuplicateKey(Box::new(c)));
                }

                let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                cur.set_key_ssq(db, coll, recordid);
                cur.set_value_u(&frame_doc_value(&key, &blob));
                cur.update()?;

                // Maintain secondary indexes as a set diff (additions first, removals
                // last — see `index_entry_diff` for why lock-free readers need that
                // order), and lazily flag any index the new doc makes multikey
                // (sticky — the old doc's array-ness is never cleared).
                if !descs.is_empty() {
                    // `update_by_id` is a bare-`_id` path (not used for timeseries), so the
                    // key is the canonical `id_key(_id)` — let the helper recompute it.
                    let (additions, removals) =
                        self.index_entry_diff(&old_doc, &doc, &descs, recordid)?;
                    self.insert_index_entries(&session, db, coll, &additions)?;
                    self.remove_index_entries(&session, db, coll, &removals)?;
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
                    // The replacement doc isn't needed after this — move it in.
                    entry.insert("o", Bson::Document(doc));
                    entry.insert("o2", Bson::Document(o2));
                    self.emit_oplog(&session, vec![entry], vec![pre])?;
                }
                Ok(true)
            })
        })
    }

    /// Delete the document with `_id == id`. Returns `false` if absent.
    pub fn delete_by_id(&self, db: &str, coll: &str, id: &Bson) -> Result<bool> {
        self.retry_write_conflicts("delete_by_id", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let key = id_key(id)?;
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                // Resolve `_id` -> RecordId (read-only; the `_id`-index row is removed
                // last, by delete_nat_entry, to keep "doc row first, entries after").
                let Some(recordid) = self.doc_recordid(&session, db, coll, &key)? else {
                    return Ok(false);
                };
                let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                cur.set_key_ssq(db, coll, recordid);
                // Read the doc first so we can retract its index entries, then remove.
                let old_blob = match cur.search() {
                    Ok(()) => {
                        let value = cur.get_value_u()?;
                        let (_idk, blob) = unframe_doc_value(&value)?;
                        blob.to_vec()
                    }
                    Err(e) if e.is_not_found() => return Ok(false),
                    Err(e) => return Err(e.into()),
                };
                cur.remove()?;
                let old_doc = decode_doc(&old_blob)?;
                let descs = self.index_descs(&session, db, coll)?;
                // `delete_by_id` is a bare-`_id` path (not used for timeseries).
                self.delete_index_entries(&session, db, coll, &old_doc, &descs, recordid)?;
                self.delete_nat_entry(&session, db, coll, &key)?;
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
            })
        })
    }

    /// Delete docs whose TTL-indexed `DateTime` field is older than `now -
    /// expireAfterSeconds`, returning the number pruned. For every index with a
    /// non-negative `expireAfterSeconds` option, the leading field is checked;
    /// docs missing the field, holding a non-date value, or inside the TTL
    /// window are left in place. The clock is injected (`now`) so tests can drive
    /// expiry — there is no background sweeper (mirrors `storage.prune_ttl`, sans
    /// the sub-phase-3 oplog emission).
    pub fn prune_ttl(&self, db: &str, coll: &str, now: bson::DateTime) -> Result<usize> {
        self.retry_write_conflicts("prune_ttl", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let session = OpSession::Fresh(self.conn.open_session()?);
            self.with_statement_txn(&session, || {
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
                let doc_cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                let mut pruned = 0usize;
                for (recordid, id_k, blob) in candidates {
                    let doc = decode_doc(&blob)?;
                    let expired = ttl.iter().any(|(field, secs)| match get_path(&doc, field) {
                        Some(Bson::DateTime(v)) => {
                            (when_ms - v.timestamp_millis()) as f64 / 1000.0 > *secs
                        }
                        _ => false,
                    });
                    if !expired {
                        continue;
                    }
                    // Doc row first, entries after: a lock-free reader hitting a stale
                    // index/nat entry skips the not-found doc, whereas removing the
                    // entries first would make an index-routed read miss a still-live
                    // doc.
                    doc_cur.reset()?;
                    doc_cur.set_key_ssq(db, coll, recordid);
                    match doc_cur.remove() {
                        Ok(()) => {}
                        Err(e) if e.is_not_found() => {}
                        Err(e) => return Err(e.into()),
                    }
                    self.delete_index_entries(&session, db, coll, &doc, &descs, recordid)?;
                    self.delete_nat_entry(&session, db, coll, &id_k)?;
                    pruned += 1;
                }
                Ok(pruned)
            })
        })
    }

    /// Run `prune_ttl` against every collection in every database, returning the
    /// total docs pruned. Per-collection errors (e.g. a concurrent drop) are
    /// suppressed so a global sweep never aborts. Mirrors
    /// `storage.prune_ttl_all_collections`.
    pub fn prune_ttl_all_collections(&self, now: bson::DateTime) -> Result<usize> {
        // Snapshot all (db, coll) under the lock, then prune each (prune_ttl
        // takes the lock itself, so it isn't held across the per-coll work).
        let pairs: Vec<(String, String)> = {
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let session = self.conn.open_session()?;
            let cur = session.open_cursor(COLL_TABLE, None)?;
            let mut pairs = Vec::new();
            let mut more = cur.next()?;
            while more {
                pairs.push(cur.get_key_ss()?);
                more = cur.next()?;
            }
            pairs
        };
        let mut total = 0usize;
        for (db, coll) in pairs {
            if let Ok(n) = self.prune_ttl(&db, &coll, now) {
                total += n;
            }
        }
        Ok(total)
    }

    pub fn collection_exists(&self, db: &str, coll: &str) -> Result<bool> {
        // Lock-free read (see the `lock` field's invariants).
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
        // Lock-free read (see the `lock` field's invariants).
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

    /// Every database that has at least one registered collection, plus `local`
    /// when the oplog is enabled (mongod always exposes it). Sorted. Mirrors
    /// `storage.list_databases`.
    pub fn list_databases(&self) -> Result<Vec<String>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(COLL_TABLE, None)?;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut more = cur.next()?;
        while more {
            let (d, _c) = cur.get_key_ss()?;
            seen.insert(d);
            more = cur.next()?;
        }
        if self.enable_oplog {
            seen.insert("local".to_string());
        }
        Ok(seen.into_iter().collect())
    }

    /// Register `(db, coll)` as an (empty) collection. Returns `false` if it
    /// already exists, `true` if created — minting its UUID and emitting an
    /// `op: "c"` `create` oplog entry. Mirrors `storage.create_collection`.
    pub fn create_collection(&self, db: &str, coll: &str) -> Result<bool> {
        self.create_collection_with_options(db, coll, &Document::new())
    }

    /// Like [`create_collection`], but persists `options` (`capped` / `size` /
    /// `max` / `validator` / `viewOn` / …) to the collection blob **and** carries
    /// them as siblings of `create` in the `c` oplog entry's `o`, so PITR replay
    /// (Rust or Python) and `show_expanded_events` create events reconstruct them.
    /// Mirrors Python `Storage.create_collection(..., options=...)`.
    pub fn create_collection_with_options(
        &self,
        db: &str,
        coll: &str,
        options: &Document,
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.op_session()?;
        if collection_registered(&session, db, coll)? {
            return Ok(false);
        }
        ensure_collection(&session, db, coll)?;
        if !options.is_empty() {
            // Persist before minting the UUID below — collection_uuid re-reads and
            // merges, so the options survive.
            let mut current = coll_options(&session, db, coll)?.unwrap_or_default();
            for (k, v) in options {
                current.insert(k.clone(), v.clone());
            }
            write_coll_options(&session, db, coll, &current)?;
        }
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let mut id_key_spec = Document::new();
            id_key_spec.insert("_id", 1i32);
            let mut id_index = Document::new();
            id_index.insert("v", 2i32);
            id_index.insert("key", Bson::Document(id_key_spec));
            id_index.insert("name", ID_INDEX_NAME);
            let mut o = Document::new();
            o.insert("create", coll);
            for (k, v) in options {
                o.insert(k.clone(), v.clone());
            }
            o.insert("idIndex", Bson::Document(id_index));
            let mut entry = Document::new();
            entry.insert("op", "c");
            entry.insert("ns", format!("{db}.$cmd"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
        Ok(true)
    }

    /// Drop a collection: delete its documents, indexes, and index entries, then
    /// its registry row. Returns whether it existed. Emits an `op: "c"` `drop`
    /// oplog entry when it existed. Mirrors `storage.drop_collection`.
    pub fn drop_collection(&self, db: &str, coll: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let existed = coll_options(&session, db, coll)?.is_some();
        let ui = if existed && self.enable_oplog {
            Some(collection_uuid(&session, db, coll)?)
        } else {
            None
        };
        self.purge_collection_tables(&session, db, coll)?;
        let c = session.open_cursor(COLL_TABLE, None)?;
        c.set_key_ss(db, coll);
        match c.search() {
            Ok(()) => c.remove()?,
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
        if let Some(ui) = ui {
            let mut o = Document::new();
            o.insert("drop", coll);
            let mut entry = Document::new();
            entry.insert("op", "c");
            entry.insert("ns", format!("{db}.$cmd"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
        Ok(existed)
    }

    /// Drop an entire database: delete every collection's data + registry rows.
    /// Emits one `op: "c"` `drop` per collection plus a final `dropDatabase: 1`
    /// command oplog entry (no `ui`). Mirrors `storage.drop_database`.
    pub fn drop_database(&self, db: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let colls = self.colls_of(&session, db)?;
        // Every existing collection's write lock (sorted registry order), so
        // in-flight CRUD on the db drains before the purge. A collection
        // created concurrently by a racing insert's lazy ensure_collection
        // isn't in this snapshot — that insert-vs-dropDatabase race is
        // observable on real mongod too and the Python server accepts the
        // same window.
        let ns_locks: Vec<_> = colls.iter().map(|c| self.coll_lock(db, c)).collect();
        let _ns_guards: Vec<_> = ns_locks
            .iter()
            .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()))
            .collect();
        let mut ui_pairs: Vec<(String, Vec<u8>)> = Vec::new();
        if self.enable_oplog {
            for c in &colls {
                ui_pairs.push((c.clone(), collection_uuid(&session, db, c)?));
            }
        }
        for c in &colls {
            self.purge_collection_tables(&session, db, c)?;
        }
        let rc = session.open_cursor(COLL_TABLE, None)?;
        for c in &colls {
            rc.reset()?;
            rc.set_key_ss(db, c);
            match rc.search() {
                Ok(()) => rc.remove()?,
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        if self.enable_oplog {
            let mut entries: Vec<Document> = Vec::new();
            for (c, ui) in &ui_pairs {
                let mut o = Document::new();
                o.insert("drop", c.clone());
                let mut entry = Document::new();
                entry.insert("op", "c");
                entry.insert("ns", format!("{db}.$cmd"));
                entry.insert("ui", uuid_binary(ui));
                entry.insert("o", Bson::Document(o));
                entries.push(entry);
            }
            let mut dd_o = Document::new();
            dd_o.insert("dropDatabase", 1i32);
            let mut dd = Document::new();
            dd.insert("op", "c");
            dd.insert("ns", format!("{db}.$cmd"));
            dd.insert("o", Bson::Document(dd_o));
            entries.push(dd);
            let n = entries.len();
            self.emit_oplog(&session, entries, vec![None; n])?;
        }
        Ok(())
    }

    /// Rename `src_db.src_coll` to `dst_db.dst_coll`, moving its document /
    /// index / entry rows. Returns `(true, None)` on success, `(false, Some(msg))`
    /// when the source is missing or the target exists without `drop_target`.
    /// The destination is registered with fresh (empty) options — its UUID is
    /// re-minted on next use (faithful to `storage.rename_collection`). Emits a
    /// `renameCollection` `op: "c"` entry (preceded by a `drop` of the target
    /// when `drop_target` replaced one).
    pub fn rename_collection(
        &self,
        src_db: &str,
        src_coll: &str,
        dst_db: &str,
        dst_coll: &str,
        drop_target: bool,
    ) -> Result<(bool, Option<String>)> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Both namespaces' write locks, in sorted key order so two renames
        // touching the same pair can't ABBA-deadlock (global first, then
        // collection locks — see `lock`'s ordering rules).
        let mut ns = vec![(src_db, src_coll), (dst_db, dst_coll)];
        ns.sort_unstable();
        ns.dedup();
        let ns_locks: Vec<_> = ns.iter().map(|(d, c)| self.coll_lock(d, c)).collect();
        let _ns_guards: Vec<_> = ns_locks
            .iter()
            .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()))
            .collect();
        let session = self.conn.open_session()?;
        if coll_options(&session, src_db, src_coll)?.is_none() {
            return Ok((
                false,
                Some(format!(
                    "source namespace does not exist: {src_db}.{src_coll}"
                )),
            ));
        }
        if (src_db, src_coll) == (dst_db, dst_coll) {
            return Ok((true, None));
        }
        let dst_existed = coll_options(&session, dst_db, dst_coll)?.is_some();
        if dst_existed && !drop_target {
            return Ok((
                false,
                Some(format!("target namespace exists: {dst_db}.{dst_coll}")),
            ));
        }
        let ui = if self.enable_oplog {
            Some(collection_uuid(&session, src_db, src_coll)?)
        } else {
            None
        };
        let dst_ui = if dst_existed && self.enable_oplog {
            Some(collection_uuid(&session, dst_db, dst_coll)?)
        } else {
            None
        };
        if dst_existed {
            self.purge_collection_tables(&session, dst_db, dst_coll)?;
            let c = session.open_cursor(COLL_TABLE, None)?;
            c.set_key_ss(dst_db, dst_coll);
            match c.search() {
                Ok(()) => c.remove()?,
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Collect every src row, drop the src tables, then re-key into dst.
        let docs = self.scan_docs(&session, src_db, src_coll)?;
        let idx_rows = self.collect_idx_rows(&session, src_db, src_coll)?;
        self.purge_collection_tables(&session, src_db, src_coll)?;
        // Sharded: dst rows go to the dst collection's shard (may differ from the
        // src shard — the src rows were already read into `docs` and purged above).
        // Re-mint a fresh RecordId per doc in src natural order (preserving
        // insertion order) and write the dst `_id` index (id_key -> RecordId); the
        // doc-table value carries the id_key in-band (framed).
        let dcur = session.open_cursor(&doc_table_for(dst_db, dst_coll), None)?;
        // Remember each doc against the RecordId it was RE-MINTED under in the
        // destination — the source RecordIds do not carry over.
        let mut moved: Vec<(i64, Document)> = Vec::with_capacity(docs.len());
        for (_src_rid, id_k, blob) in &docs {
            let recordid = self.write_nat_entry(&session, dst_db, dst_coll, id_k)?;
            dcur.reset()?;
            dcur.set_key_ssq(dst_db, dst_coll, recordid);
            dcur.set_value_u(&frame_doc_value(id_k, blob));
            dcur.insert()?;
            moved.push((recordid, decode_doc(blob)?));
        }
        let icur = session.open_cursor(IDX_TABLE, None)?;
        for (name, payload) in &idx_rows {
            icur.reset()?;
            icur.set_key_sss(dst_db, dst_coll, name);
            icur.set_value_u(payload);
            icur.insert()?;
        }
        // REBUILD the index entries rather than copying the source's packed rows.
        // Step-2 entries carry the RecordId, and rename re-mints every RecordId, so
        // copied entries would point at rows that do not exist in the destination —
        // silently breaking every index on the renamed collection. (Under step 1
        // entries carried the `id_key`, which survives a rename, so the copy was
        // safe then.) The index catalog rows are written above, so `index_descs`
        // sees the destination's indexes here.
        let dst_descs = self.index_descs(&session, dst_db, dst_coll)?;
        for (recordid, doc) in &moved {
            self.write_index_entries(&session, dst_db, dst_coll, doc, &dst_descs, *recordid)?;
        }
        ensure_collection(&session, dst_db, dst_coll)?;
        let rc = session.open_cursor(COLL_TABLE, None)?;
        rc.set_key_ss(src_db, src_coll);
        match rc.search() {
            Ok(()) => rc.remove()?,
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
        if self.enable_oplog {
            let mut entries: Vec<Document> = Vec::new();
            if let Some(du) = &dst_ui {
                let mut o = Document::new();
                o.insert("drop", dst_coll);
                let mut e = Document::new();
                e.insert("op", "c");
                e.insert("ns", format!("{dst_db}.$cmd"));
                e.insert("ui", uuid_binary(du));
                e.insert("o", Bson::Document(o));
                entries.push(e);
            }
            let mut o = Document::new();
            o.insert("renameCollection", format!("{src_db}.{src_coll}"));
            o.insert("to", format!("{dst_db}.{dst_coll}"));
            // mongod records the dropped target's UUID under `dropTarget` in the
            // rename oplog entry; the change-stream `rename` event surfaces it as
            // `operationDescription.dropTarget` when `showExpandedEvents` is on.
            if let Some(du) = &dst_ui {
                o.insert("dropTarget", uuid_binary(du));
            }
            let mut e = Document::new();
            e.insert("op", "c");
            e.insert("ns", format!("{src_db}.$cmd"));
            if let Some(u) = &ui {
                e.insert("ui", uuid_binary(u));
            }
            e.insert("o", Bson::Document(o));
            entries.push(e);
            let n = entries.len();
            self.emit_oplog(&session, entries, vec![None; n])?;
        }
        Ok((true, None))
    }

    /// The collection's options document (`{}` if it doesn't exist).
    /// `local.oplog.rs` reports the synthetic capped shape mongod uses. The
    /// stored `uuid` stays a BSON Binary (the command layer decodes it). Mirrors
    /// `storage.get_collection_options`.
    pub fn get_collection_options(&self, db: &str, coll: &str) -> Result<Document> {
        if self.enable_oplog && db == "local" && coll == "oplog.rs" {
            let mut o = Document::new();
            o.insert("capped", true);
            o.insert("size", (self.oplog_max_entries as i64) * 16 * 1024);
            o.insert("max", self.oplog_max_entries as i64);
            return Ok(o);
        }
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        Ok(coll_options(&session, db, coll)?.unwrap_or_default())
    }

    /// Whether the collection has `capped: true`. Mirrors
    /// `storage.collection_is_capped`.
    pub fn collection_is_capped(&self, db: &str, coll: &str) -> Result<bool> {
        // The synthetic oplog view is a capped collection (so tailable cursors
        // are accepted on it), even though it has no registry row.
        if self.is_oplog_rs(db, coll) {
            return Ok(true);
        }
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        Ok(coll_options(&session, db, coll)?
            .map(|o| o.get_bool("capped").unwrap_or(false))
            .unwrap_or(false))
    }

    /// Sum of BSON-encoded document bytes for the collection (the `size` /
    /// `dataSize` `collStats` reports). Best-effort — excludes WT block
    /// overhead. Mirrors `storage.collection_data_size`.
    pub fn collection_data_size(&self, db: &str, coll: &str) -> Result<i64> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let mut total = 0i64;
        for (_rid, _id_k, blob) in self.scan_docs(&session, db, coll)? {
            total += blob.len() as i64;
        }
        Ok(total)
    }

    /// Per-index byte size as a `{name: bytes}` document: `_id_` is the summed
    /// `id_key` length over the doc table; each secondary index is its summed
    /// packed-entry length. Mirrors `storage.index_sizes`.
    pub fn index_sizes(&self, db: &str, coll: &str) -> Result<Document> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let mut out = Document::new();
        let mut id_size = 0i64;
        for (_rid, id_k, _blob) in self.scan_docs(&session, db, coll)? {
            id_size += id_k.len() as i64;
        }
        if id_size > 0 {
            out.insert(ID_INDEX_NAME, id_size);
        }
        for (name, packed) in self.collect_entry_rows(&session, db, coll)? {
            let prev = out.get_i64(&name).unwrap_or(0);
            out.insert(name, prev + packed.len() as i64);
        }
        Ok(out)
    }

    /// Documents whose `id_key` is strictly greater than `after` (the whole
    /// collection when `after` is `None`), in natural order, as
    /// `(id_key, blob)`. Used by the tailable-cursor producer to emit only the
    /// docs inserted since the last poll. Mirrors
    /// `storage.scan_docs_after_id_key`.
    pub fn scan_docs_after_id_key(
        &self,
        db: &str,
        coll: &str,
        after: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let rows = self
            .scan_docs(&session, db, coll)?
            .into_iter()
            .map(|(_rid, id_k, blob)| (id_k, blob));
        Ok(match after {
            None => rows.collect(),
            Some(a) => rows.filter(|(id_k, _)| id_k.as_slice() > a).collect(),
        })
    }

    /// The smallest `id_key` currently in a collection, or `None` if empty. A
    /// tailable cursor uses this to detect capped rollover: if the doc it last
    /// returned has been evicted (min `id_key` now exceeds the cursor's anchor),
    /// mongod kills the cursor with `CappedPositionLost`.
    pub fn collection_min_id_key(&self, db: &str, coll: &str) -> Result<Option<Vec<u8>>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let rows = self.scan_docs(&session, db, coll)?;
        Ok(rows.into_iter().map(|(_rid, k, _)| k).min())
    }

    /// Doc blobs whose **RecordId** is greater than `after` (or all, `after=None`),
    /// in RecordId (insertion) order, each paired with its RecordId. This is the
    /// tailable capped-cursor scan: the doc table is keyed by the monotonic
    /// RecordId, so insertion order — the order mongod's tailable cursors follow —
    /// IS RecordId order. (`scan_docs_after_id_key` filters by `id_key` instead,
    /// which only coincides with insertion order for monotonic `_id`s — wrong for
    /// a capped collection with custom non-monotonic `_id`s, where a later insert
    /// can carry a smaller `id_key`. That method stays for its non-tailable
    /// callers; tailable follows this one.)
    pub fn scan_docs_after_recordid(
        &self,
        db: &str,
        coll: &str,
        after: Option<i64>,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let rows = self
            .scan_docs(&session, db, coll)?
            .into_iter()
            .map(|(rid, _id_k, blob)| (rid, blob));
        Ok(match after {
            None => rows.collect(),
            Some(a) => rows.filter(|(rid, _)| *rid > a).collect(),
        })
    }

    /// The smallest **RecordId** currently in a collection, or `None` if empty. A
    /// tailable cursor uses this to detect capped rollover: capped eviction is
    /// FIFO by RecordId (`enforce_capped_bounds`), so if the min RecordId now
    /// exceeds the cursor's RecordId anchor, the doc it last returned has been
    /// evicted and mongod kills the cursor with `CappedPositionLost`. (The
    /// `id_key`-based `collection_min_id_key` mis-detects this for non-monotonic
    /// `_id`s, since the evicted doc need not hold the min `id_key`.)
    pub fn collection_min_recordid(&self, db: &str, coll: &str) -> Result<Option<i64>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        // `scan_docs` yields RecordId-ascending, so the first row is the minimum.
        Ok(self
            .scan_docs(&session, db, coll)?
            .into_iter()
            .next()
            .map(|(rid, _idk, _blob)| rid))
    }

    /// The largest **RecordId** currently in a collection (`None` if empty). A
    /// tailable cursor seeds its watermark with this: after the initial find hands
    /// out the current contents, the producer follows only docs inserted
    /// afterward — i.e. with a RecordId strictly greater than the collection's max
    /// at setup. This is where mongod positions a tailable cursor (end of the
    /// initial scan), and for a monotonic-`_id` capped collection it equals the
    /// last-returned doc's position, so existing behaviour is unchanged.
    pub fn collection_max_recordid(&self, db: &str, coll: &str) -> Result<Option<i64>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        // `scan_docs` yields RecordId-ascending, so the last row is the maximum.
        Ok(self
            .scan_docs(&session, db, coll)?
            .into_iter()
            .next_back()
            .map(|(rid, _idk, _blob)| rid))
    }

    // --- users / roles / profiling (auth + profiling surface) ---

    /// Persist a user record (opaque BSON blob). Returns `true` if added,
    /// `false` if it already existed and `replace` is false. Mirrors
    /// `storage.add_user`.
    pub fn add_user(&self, db: &str, username: &str, record: &[u8], replace: bool) -> Result<bool> {
        self.put_ss_record(USERS_TABLE, db, username, record, replace)
    }

    /// Fetch a user record (BSON blob) or `None`. Mirrors `storage.get_user`.
    pub fn get_user(&self, db: &str, username: &str) -> Result<Option<Vec<u8>>> {
        self.get_ss_record(USERS_TABLE, db, username)
    }

    /// Delete a user. Returns `false` if absent. Mirrors `storage.drop_user`.
    pub fn drop_user(&self, db: &str, username: &str) -> Result<bool> {
        self.drop_ss_record(USERS_TABLE, db, username)
    }

    /// Paginated user listing (`db = None` spans every database). Mirrors
    /// `storage.list_users`.
    pub fn list_users(&self, db: Option<&str>, skip: usize, limit: usize) -> Result<Vec<Vec<u8>>> {
        self.list_ss_records(USERS_TABLE, db, skip, limit)
    }

    /// Persist a custom-role record (opaque BSON blob). Mirrors
    /// `storage.add_role`.
    pub fn add_role(&self, db: &str, name: &str, record: &[u8], replace: bool) -> Result<bool> {
        self.put_ss_record(ROLES_TABLE, db, name, record, replace)
    }

    /// Fetch a custom-role record (BSON blob) or `None`. Mirrors
    /// `storage.get_role`.
    pub fn get_role(&self, db: &str, name: &str) -> Result<Option<Vec<u8>>> {
        self.get_ss_record(ROLES_TABLE, db, name)
    }

    /// Delete a custom role. Returns `false` if absent. Mirrors
    /// `storage.drop_role`.
    pub fn drop_role(&self, db: &str, name: &str) -> Result<bool> {
        self.drop_ss_record(ROLES_TABLE, db, name)
    }

    /// Paginated custom-role listing (`db = None` spans every database).
    /// Mirrors `storage.list_roles`.
    pub fn list_roles(&self, db: Option<&str>, skip: usize, limit: usize) -> Result<Vec<Vec<u8>>> {
        self.list_ss_records(ROLES_TABLE, db, skip, limit)
    }

    /// The active profile settings for `db` (`{level, slowms, sampleRate}`),
    /// defaulting to mongod's `level 0 / slowms 100 / sampleRate 1.0` when
    /// unset. Mirrors `storage.get_profile`.
    pub fn get_profile(&self, db: &str) -> Result<Document> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(PROFILE_TABLE, None)?;
        cur.set_key_s(db);
        let doc = match cur.search() {
            Ok(()) => {
                let blob = cur.get_value_u()?;
                if blob.is_empty() {
                    None
                } else {
                    Some(decode_doc(&blob)?)
                }
            }
            Err(e) if e.is_not_found() => None,
            Err(e) => return Err(e.into()),
        };
        let stored = doc.unwrap_or_default();
        let level = stored.get_i32("level").unwrap_or(0);
        let slowms = stored.get_i32("slowms").unwrap_or(100);
        let rate = match stored.get("sampleRate") {
            Some(Bson::Double(r)) => *r,
            Some(Bson::Int32(r)) => f64::from(*r),
            Some(Bson::Int64(r)) => *r as f64,
            _ => 1.0,
        };
        let mut out = Document::new();
        out.insert("level", level);
        out.insert("slowms", slowms);
        out.insert("sampleRate", rate);
        Ok(out)
    }

    /// Persist profile settings for `db`. `level` must be 0/1/2, `slowms`
    /// non-negative, `sample_rate` in `[0, 1]` (else `BadValue`). Mirrors
    /// `storage.set_profile`.
    pub fn set_profile(&self, db: &str, level: i32, slowms: i32, sample_rate: f64) -> Result<()> {
        if !(0..=2).contains(&level) {
            return Err(StorageError::BadHint("level must be 0, 1, or 2".into()));
        }
        if slowms < 0 {
            return Err(StorageError::BadHint("slowms must be non-negative".into()));
        }
        if !(0.0..=1.0).contains(&sample_rate) {
            return Err(StorageError::BadHint("sampleRate must be in [0, 1]".into()));
        }
        let mut doc = Document::new();
        doc.insert("level", level);
        doc.insert("slowms", slowms);
        doc.insert("sampleRate", sample_rate);
        let blob = encode_doc(&doc)?;
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(PROFILE_TABLE, None)?;
        cur.set_key_s(db);
        cur.set_value_u(&blob);
        cur.insert()?;
        Ok(())
    }

    /// Ensure `<db>.system.profile` exists as a capped collection (default
    /// 10 MB). Mirrors `storage.ensure_profile_collection`. Takes no outer lock
    /// — delegates to the (individually locked) public methods.
    pub fn ensure_profile_collection(&self, db: &str, size_bytes: i64) -> Result<()> {
        if self.collection_exists(db, "system.profile")? {
            return Ok(());
        }
        self.create_collection(db, "system.profile")?;
        let mut opts = Document::new();
        opts.insert("capped", true);
        opts.insert("size", size_bytes);
        self.set_collection_options(db, "system.profile", &opts)
    }

    /// Insert-or-replace a `(db, name) -> blob` record in an `SS`-keyed table.
    /// Returns `false` (no write) if the key exists and `replace` is false.
    fn put_ss_record(
        &self,
        table: &str,
        db: &str,
        name: &str,
        record: &[u8],
        replace: bool,
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let probe = session.open_cursor(table, None)?;
        probe.set_key_ss(db, name);
        let exists = match probe.search() {
            Ok(()) => true,
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        if exists && !replace {
            return Ok(false);
        }
        let cur = session.open_cursor(table, None)?;
        cur.set_key_ss(db, name);
        cur.set_value_u(record);
        cur.insert()?;
        Ok(true)
    }

    /// Point-fetch a `(db, name)` blob from an `SS`-keyed table (`None` when
    /// absent or empty).
    fn get_ss_record(&self, table: &str, db: &str, name: &str) -> Result<Option<Vec<u8>>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // Fresh session for cross-thread visibility (mirrors `get_role`).
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(table, None)?;
        cur.set_key_ss(db, name);
        match cur.search() {
            Ok(()) => {
                let blob = cur.get_value_u()?;
                Ok(if blob.is_empty() { None } else { Some(blob) })
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a `(db, name)` row from an `SS`-keyed table (`false` if absent).
    fn drop_ss_record(&self, table: &str, db: &str, name: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(table, None)?;
        cur.set_key_ss(db, name);
        match cur.search() {
            Ok(()) => {
                cur.remove()?;
                Ok(true)
            }
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Paginated walk of an `SS`-keyed table (`db = None` spans all dbs),
    /// returning the non-empty value blobs in natural key order.
    fn list_ss_records(
        &self,
        table: &str,
        db: Option<&str>,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<Vec<u8>>> {
        let limit = if limit == 0 || limit > 1000 {
            1000
        } else {
            limit
        };
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(table, None)?;
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut seen = 0usize;
        let mut more = cur.next()?;
        while more {
            let (row_db, _name) = cur.get_key_ss()?;
            if db.is_none() || db == Some(row_db.as_str()) {
                if seen >= skip {
                    let blob = cur.get_value_u()?;
                    if !blob.is_empty() {
                        out.push(blob);
                    }
                    if out.len() >= limit {
                        break;
                    }
                }
                seen += 1;
            }
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
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        if name == ID_INDEX_NAME {
            return Ok(false);
        }
        // `2d` (point-only geohash) and `2dsphere` (S2 cell) geo indexes are
        // supported. Other non-numeric index types (text / hashed) are rejected.
        let geo = parse_geo_2d(key_spec, options);
        let geo_sphere = parse_geo_sphere(key_spec);
        if geo.is_none() && geo_sphere.is_none() {
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

        // `op_session` (not a fresh session) so `createIndexes` inside a
        // multi-document transaction runs on the transaction's WT session — a
        // fresh session would deadlock against the same transaction's
        // uncommitted writes (e.g. a collection created earlier in the txn).
        let session = self.op_session()?;
        ensure_collection(&session, db, coll)?;

        let c = session.open_cursor(IDX_TABLE, None)?;
        c.set_key_sss(db, coll, name);
        match c.search() {
            Ok(()) => {
                // Index exists: reject a conflicting key spec or options, else
                // no-op success.
                let existing = decode_doc(&c.get_value_u()?)?;
                // Same name, different key spec → IndexKeySpecsConflict (86).
                // Order-sensitive: mongod treats {a:1,b:1} and {b:1,a:1} as
                // distinct indexes, so compare the ordered field lists.
                let existing_key = existing.get_document("key").ok();
                let same_key = existing_key.is_some_and(|k| index_keys_equiv(k, key_spec));
                if !same_key {
                    return Err(StorageError::IndexKeySpecsConflict(format!(
                        "An existing index has the same name as the requested index. \
                         Requested index: {{ key: {key_spec:?}, name: {name:?} }}, \
                         existing index: {{ key: {:?}, name: {name:?} }}",
                        existing_key.cloned().unwrap_or_default()
                    )));
                }
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
        // Stamp the entry format so a later build can tell step-2 entries from
        // step-1 ones. Not a user option: `listIndexes` strips it (like
        // `multikey`), and the options-conflict check compares only the
        // enumerated user-facing options, so it never provokes a false conflict.
        stored_options.insert("entryFormat", ENTRY_FORMAT_RECORDID);
        let entries: Vec<(Vec<u8>, i64)> = if let Some(geo) = &geo {
            // 2d geo index: one geohash cell per point-valued doc. Always flagged
            // multikey so the regular (numeric) pickers skip it.
            stored_options.insert("multikey", Bson::Boolean(true));
            let mut out: Vec<(Vec<u8>, i64)> = Vec::new();
            for (rid, _id_k, blob) in self.scan_docs(&session, db, coll)? {
                let d = decode_doc(&blob)?;
                if let Some(kb) = get_path(&d, &geo.field).and_then(|v| geo.cell_kb(v)) {
                    out.push((kb, rid));
                }
            }
            out
        } else if let Some(gs) = &geo_sphere {
            // 2dsphere S2 index: covering cells + ancestors per geometry-valued
            // doc. Flagged multikey (one doc → many cell entries).
            stored_options.insert("multikey", Bson::Boolean(true));
            let mut out: Vec<(Vec<u8>, i64)> = Vec::new();
            for (rid, _id_k, blob) in self.scan_docs(&session, db, coll)? {
                let d = decode_doc(&blob)?;
                if let Some(v) = get_path(&d, &gs.field) {
                    for kb in gs.cell_kbs(v) {
                        out.push((kb, rid));
                    }
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
            // Entries carry the doc's RecordId (step 2), which `scan_docs`
            // already hands us — no `id_key -> RecordId` lookup needed here.
            let mut entries: Vec<(Vec<u8>, i64)> = Vec::new();
            let mut seen: HashSet<Vec<u8>> = HashSet::new();
            for (rid, _id_k, blob) in self.scan_docs(&session, db, coll)? {
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
                for kb in index_key_variants(&d, key_spec, sparse)? {
                    // Uniqueness is checked against the same key variants the
                    // entries are built from — mongod's unique-multikey rule is
                    // "no two docs share any generated key".
                    if unique && !seen.insert(kb.clone()) {
                        // A pre-existing doc already holds this key — can't
                        // build a unique index over the data.
                        return Err(StorageError::DuplicateKey(Box::new(UniqueConflict {
                            index: name.to_string(),
                            key_pattern: key_spec.clone(),
                            key_value: conflict_key_value(&d, key_spec, &kb),
                        })));
                    }
                    entries.push((kb, rid));
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

        // Backfill the entry rows BEFORE the registry row: lock-free readers
        // route through an index the moment its registry row is visible, so
        // the registry insert must be the commit point of a fully-built
        // index — the reverse order let a reader route through a half-backfilled
        // index and miss matching documents. (`drop_index` is the mirror
        // image: registry row out first, then the entries.)
        let ec = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (kb, rid) in &entries {
            let packed = pack_entry(kb, *rid);
            ec.reset()?;
            ec.set_key_sssu(db, coll, name, &packed);
            ec.set_value_u(b"");
            ec.insert()?;
        }

        c.reset()?;
        c.set_key_sss(db, coll, name);
        c.set_value_u(&payload);
        c.insert()?;
        // Oplog: a DDL `op: "c"` `createIndexes` entry so a `showExpandedEvents`
        // change stream surfaces a `createIndexes` event (the projector reads
        // `o.createIndexes` + `o.indexes[].{v,key,name}`).
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let mut idx = Document::new();
            idx.insert("v", 2i32);
            idx.insert("key", Bson::Document(key_spec.clone()));
            idx.insert("name", name);
            let mut o = Document::new();
            o.insert("createIndexes", coll);
            o.insert("indexes", Bson::Array(vec![Bson::Document(idx)]));
            let mut entry = Document::new();
            entry.insert("op", "c");
            entry.insert("ns", format!("{db}.$cmd"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
        Ok(true)
    }

    /// All indexes on `(db, coll)` in MongoDB's `listIndexes` shape (the virtual
    /// `_id_` index first, then stored indexes), sorted by name. Empty when the
    /// collection doesn't exist. Mirrors `storage.list_indexes`.
    pub fn list_indexes(&self, db: &str, coll: &str) -> Result<Vec<Document>> {
        // Lock-free read (see the `lock` field's invariants).
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

    /// The sticky multikey flag for index `name` (false if unknown). The `_id`
    /// index can never be multikey — the write path rejects an array `_id`.
    /// Mirrors `storage.index_is_multikey`.
    pub fn index_is_multikey(&self, db: &str, coll: &str, name: &str) -> bool {
        if name == ID_INDEX_NAME {
            return false;
        }
        // Lock-free read (see the `lock` field's invariants).
        let Ok(session) = self.conn.open_session() else {
            return false;
        };
        let Ok(indexes) = self.iter_indexes(&session, db, coll) else {
            return false;
        };
        indexes
            .into_iter()
            .any(|(n, _key_spec, opts)| n == name && opts.get_bool("multikey").unwrap_or(false))
    }

    /// Retune a TTL index's `expireAfterSeconds` option, resolving the index by
    /// name. Returns `false` if there is no such index. Used by `collMod` replay
    /// (`{index: {name, expireAfterSeconds}}`). Mirrors `storage.set_index_expiry`.
    pub fn set_index_expiry(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        expire_after_seconds: i64,
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(IDX_TABLE, None)?;
        cur.set_key_sss(db, coll, name);
        match cur.search() {
            Ok(()) => {}
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        let mut payload = decode_doc(&cur.get_value_u()?)?;
        let mut opts = payload.get_document("options").cloned().unwrap_or_default();
        opts.insert("expireAfterSeconds", expire_after_seconds);
        payload.insert("options", Bson::Document(opts));
        let blob = encode_doc(&payload)?;
        let wcur = session.open_cursor(IDX_TABLE, None)?;
        wcur.set_key_sss(db, coll, name);
        wcur.set_value_u(&blob);
        wcur.update()?;
        Ok(true)
    }

    /// Merge `new_opts` into an existing index's stored options blob
    /// (read-modify-write, like `set_index_expiry`). Backs `collMod {index:
    /// {keyPattern|name, prepareUnique|unique: ...}}`. Returns `true` when the
    /// index existed and was updated. Mirrors `storage.set_index_options`.
    pub fn set_index_options(
        &self,
        db: &str,
        coll: &str,
        name: &str,
        new_opts: &Document,
    ) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let cur = session.open_cursor(IDX_TABLE, None)?;
        cur.set_key_sss(db, coll, name);
        match cur.search() {
            Ok(()) => {}
            Err(e) if e.is_not_found() => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        let mut payload = decode_doc(&cur.get_value_u()?)?;
        let mut opts = payload.get_document("options").cloned().unwrap_or_default();
        for (k, v) in new_opts {
            opts.insert(k.clone(), v.clone());
        }
        payload.insert("options", Bson::Document(opts));
        let blob = encode_doc(&payload)?;
        let wcur = session.open_cursor(IDX_TABLE, None)?;
        wcur.set_key_sss(db, coll, name);
        wcur.set_value_u(&blob);
        wcur.update()?;
        Ok(true)
    }

    /// Group the `_id`s of documents that share the same key on index `name`,
    /// returning one `_id` list per duplicated key (groups of size >= 2,
    /// `_id`-sorted within each group, in first-seen key order). A non-empty
    /// result means a `collMod {index: {unique: true}}` conversion must be
    /// refused with `CannotConvertIndexToUnique` (359) and these reported as
    /// `violations`. Mirrors `storage.find_index_duplicates`.
    pub fn find_index_duplicates(
        &self,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<Vec<Vec<Bson>>> {
        // Lock-free read (see the `lock` field's invariants).
        let session = self.conn.open_session()?;
        let descs = self.index_descs(&session, db, coll)?;
        let desc = match descs.iter().find(|d| d.name == name) {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };
        let mut index_of: std::collections::HashMap<Vec<u8>, usize> =
            std::collections::HashMap::new();
        let mut groups: Vec<Vec<Bson>> = Vec::new();
        for (_rid, _id_k, blob) in self.scan_docs(&session, db, coll)? {
            let doc = decode_doc(&blob)?;
            let id = doc.get("_id").cloned().unwrap_or(Bson::Null);
            // Grouped over every key the doc contributes (sparse: none),
            // matching what `unique_conflict` would refuse — on a multikey
            // index two docs collide as soon as they share one generated key.
            for kb in index_key_variants(&doc, &desc.key_spec, desc.sparse)? {
                match index_of.get(&kb) {
                    Some(&i) => groups[i].push(id.clone()),
                    None => {
                        index_of.insert(kb, groups.len());
                        groups.push(vec![id.clone()]);
                    }
                }
            }
        }
        let mut out: Vec<Vec<Bson>> = Vec::new();
        for mut ids in groups {
            if ids.len() > 1 {
                ids.sort_by(|a, b| {
                    let ka = sortkey::encode_value(a, None).unwrap_or_default();
                    let kbk = sortkey::encode_value(b, None).unwrap_or_default();
                    ka.cmp(&kbk)
                });
                out.push(ids);
            }
        }
        Ok(out)
    }

    /// Drop the index named `name` (and all its entries). Returns `false` if no
    /// such index, or `name == "_id_"`. Mirrors `storage.drop_index`.
    pub fn drop_index(&self, db: &str, coll: &str, name: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
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
        // Capture the spec before removal: mongod's showExpandedEvents
        // `dropIndexes` event describes the dropped index in full
        // (`{v, key, name}`, probed 7.0.12), not just its name.
        let key_spec = decode_doc(&c.get_value_u()?)
            .ok()
            .and_then(|d| d.get_document("key").ok().cloned())
            .unwrap_or_default();
        c.remove()?;
        self.delete_entries_prefix(&session, db, coll, name)?;
        // Oplog: a DDL `op: "c"` `dropIndexes` entry so a `showExpandedEvents`
        // change stream surfaces a `dropIndexes` event (the projector reads
        // `o.dropIndexes` + `o.index` + `o.key`).
        if self.enable_oplog {
            let ui = collection_uuid(&session, db, coll)?;
            let mut o = Document::new();
            o.insert("dropIndexes", coll);
            o.insert("index", name);
            o.insert("key", Bson::Document(key_spec));
            let mut entry = Document::new();
            entry.insert("op", "c");
            entry.insert("ns", format!("{db}.$cmd"));
            entry.insert("ui", uuid_binary(&ui));
            entry.insert("o", Bson::Document(o));
            self.emit_oplog(&session, vec![entry], vec![None])?;
        }
        Ok(true)
    }

    /// Drop every (non-`_id_`) index on `(db, coll)`. Returns how many were
    /// dropped. Mirrors `storage.drop_all_indexes` (used by drop-collection).
    pub fn drop_all_indexes(&self, db: &str, coll: &str) -> Result<usize> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // DDL excludes in-flight CRUD on this namespace (global first,
        // then the collection lock — see `lock`'s ordering rules).
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        let dropped: Vec<(String, Document)> = self
            .iter_indexes(&session, db, coll)?
            .into_iter()
            .map(|(n, key_spec, _)| (n, key_spec))
            .collect();
        for (name, _) in &dropped {
            let c = session.open_cursor(IDX_TABLE, None)?;
            c.set_key_sss(db, coll, name);
            if c.search().is_ok() {
                c.remove()?;
            }
            self.delete_entries_prefix(&session, db, coll, name)?;
        }
        // Oplog: one `dropIndexes` "c" entry per dropped index (mongod emits
        // per-index events for `dropIndexes: "*"` too), each carrying the key
        // spec for the showExpandedEvents event's full index description.
        if self.enable_oplog && !dropped.is_empty() {
            let ui = collection_uuid(&session, db, coll)?;
            let mut entries = Vec::with_capacity(dropped.len());
            for (name, key_spec) in &dropped {
                let mut o = Document::new();
                o.insert("dropIndexes", coll);
                o.insert("index", name.as_str());
                o.insert("key", Bson::Document(key_spec.clone()));
                let mut entry = Document::new();
                entry.insert("op", "c");
                entry.insert("ns", format!("{db}.$cmd"));
                entry.insert("ui", uuid_binary(&ui));
                entry.insert("o", Bson::Document(o));
                entries.push(entry);
            }
            let n = entries.len();
            self.emit_oplog(&session, entries, vec![None; n])?;
        }
        Ok(dropped.len())
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

    /// `(RecordId, id_key, doc_bytes)` for every document in `(db, coll)`, in
    /// natural (insertion) order. The doc table is keyed by the monotonic RecordId
    /// so a forward walk IS insertion order; the `id_key` and blob come from
    /// unframing each value (see `frame_doc_value`) — no `_id` decode needed, and
    /// it recovers a timeseries doc's non-derivable suffixed id_key too.
    fn scan_docs(&self, session: &Session, db: &str, coll: &str) -> Result<Vec<ScannedDoc>> {
        let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        let mut out = Vec::new();
        cur.set_key_ssq(db, coll, i64::MIN);
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
            let (d, c, recordid) = cur.get_key_ssq()?;
            if d != db || c != coll {
                break;
            }
            let value = cur.get_value_u()?;
            let (idk, blob) = unframe_doc_value(&value)?;
            out.push((recordid, idk.to_vec(), blob.to_vec()));
            more = cur.next()?;
        }
        Ok(out)
    }

    // --- natural-order (insertion) index maintenance + scan ---

    /// Reserve the next monotonic insertion `seq`. Mirrors `storage._mint_nat_seq`.
    fn mint_nat_seq(&self) -> i64 {
        let mut st = self.oplog.lock().unwrap_or_else(|e| e.into_inner());
        let seq = st.next_nat_seq;
        st.next_nat_seq += 1;
        seq
    }

    /// Record a doc's insertion position: `seq -> id_key` plus the reverse
    /// `id_key -> seq`. Mirrors `storage._write_nat_entry`.
    /// Assign the doc a RecordId (monotonic insertion seq) and write the `_id`
    /// index row (`id_key -> RecordId`). Returns the RecordId — the caller keys
    /// the doc table by it. The forward `NAT_TABLE` (seq -> id_key) is gone: the
    /// doc table is itself in RecordId (= insertion) order.
    fn write_nat_entry(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        id_key: &[u8],
    ) -> Result<i64> {
        let recordid = self.mint_nat_seq();
        // overwrite=false: the `_id` index is where a duplicate `_id` is now caught
        // (the doc table is keyed by the unique RecordId, so it can't reject dups).
        // A wasted RecordId on the dup path is harmless — RecordIds only need to be
        // unique + monotonic; gaps are fine.
        let rev = session.open_cursor(NAT_SEQ_TABLE, Some("overwrite=false"))?;
        rev.set_key_ssu(db, coll, id_key);
        rev.set_value_q(recordid);
        match rev.insert() {
            Ok(()) => Ok(recordid),
            Err(e) if e.is_duplicate_key() => Err(StorageError::DuplicateId),
            Err(e) => Err(e.into()),
        }
    }

    /// The `_id` index lookup: resolve a doc's `id_key` to its RecordId (the
    /// doc-table key). `None` if the doc doesn't exist.
    fn doc_recordid(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        id_key: &[u8],
    ) -> Result<Option<i64>> {
        let rev = session.open_cursor(NAT_SEQ_TABLE, None)?;
        rev.set_key_ssu(db, coll, id_key);
        match rev.search() {
            Ok(()) => Ok(Some(rev.get_value_q()?)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Drop a doc's insertion-order entry (both directions). No-op if absent
    /// (legacy docs predating the index). Mirrors `storage._delete_nat_entry`.
    fn delete_nat_entry(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        id_key: &[u8],
    ) -> Result<Option<i64>> {
        // Remove the `_id` index row and return the RecordId it mapped to, so the
        // caller can delete the doc-table row keyed by it. (No forward NAT_TABLE
        // row to remove — the doc table itself is in RecordId order.)
        let rev = session.open_cursor(NAT_SEQ_TABLE, None)?;
        rev.set_key_ssu(db, coll, id_key);
        let recordid = match rev.search() {
            Ok(()) => rev.get_value_q()?,
            Err(e) if e.is_not_found() => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        rev.remove()?;
        Ok(Some(recordid))
    }

    /// Drop every natural-order entry for `(db, coll)` (both directions) — called
    /// on drop / rename so a later re-create can't resurrect stale positions.
    fn drop_nat_collection(&self, session: &Session, db: &str, coll: &str) -> Result<()> {
        // Forward (db, coll, seq): collect this collection's seqs, then remove.
        let nat = session.open_cursor(NAT_TABLE, None)?;
        nat.set_key_ssq(db, coll, i64::MIN);
        let mut more = match nat.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    nat.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        let mut seqs: Vec<i64> = Vec::new();
        while more {
            let (d, c, seq) = nat.get_key_ssq()?;
            if d != db || c != coll {
                break;
            }
            seqs.push(seq);
            more = nat.next()?;
        }
        for seq in seqs {
            nat.set_key_ssq(db, coll, seq);
            if nat.search().is_ok() {
                nat.remove()?;
            }
        }
        // Reverse (db, coll, id_key).
        let rev = session.open_cursor(NAT_SEQ_TABLE, None)?;
        rev.set_key_ssu(db, coll, b"");
        let mut more = match rev.search_near() {
            Ok(cmp) => {
                if cmp < 0 {
                    rev.next()?
                } else {
                    true
                }
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        let mut keys: Vec<Vec<u8>> = Vec::new();
        while more {
            let (d, c, k) = rev.get_key_ssu()?;
            if d != db || c != coll {
                break;
            }
            keys.push(k);
            more = rev.next()?;
        }
        for k in keys {
            rev.set_key_ssu(db, coll, &k);
            if rev.search().is_ok() {
                rev.remove()?;
            }
        }
        Ok(())
    }

    /// All doc blobs of a collection in **insertion order**. The doc table is now
    /// keyed by the monotonic RecordId, so a plain doc-table walk (`scan_docs`) IS
    /// insertion order — no separate `NAT_TABLE` indirection. Mirrors
    /// `storage._scan_docs_natural`.
    fn scan_blobs_natural(&self, session: &Session, db: &str, coll: &str) -> Result<Vec<Vec<u8>>> {
        // The read path only needs the document blobs, so walk the doc table
        // directly and clone just the blob — `scan_docs` additionally clones each
        // row's `id_key` (for the write/index paths), a per-document allocation
        // wasted on a full scan of N documents.
        let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        let mut out = Vec::new();
        cur.set_key_ssq(db, coll, i64::MIN);
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
            let (d, c, _recordid) = cur.get_key_ssq()?;
            if d != db || c != coll {
                break;
            }
            // Reuse the value's own allocation: `get_value_u` already copied
            // WiredTiger's bytes into an owned `Vec`, so drain the frame prefix
            // (`[u32 id_key_len][id_key]`) in place to leave just the blob —
            // avoiding the second allocation a `blob.to_vec()` would make.
            let mut value = cur.get_value_u()?;
            let prefix = frame_prefix_len(&value)?;
            value.drain(..prefix);
            out.push(value);
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Evict oldest non-fresh docs from a capped collection until within its
    /// `size` (byte) and `max` (count) bounds. "Oldest" is natural (insertion
    /// `seq`) order via `scan_docs_natural`, so FIFO holds even for non-monotonic
    /// custom `_id`s. Appends a `"d"` oplog entry (and pre-image when enabled) per
    /// eviction to the caller's vectors. No-op when the collection isn't capped or
    /// has no bounds. Mirrors `storage._enforce_capped_bounds_locked`.
    #[allow(clippy::too_many_arguments)]
    fn enforce_capped_bounds(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        fresh_id_keys: &HashSet<Vec<u8>>,
        descs: &[IndexDesc],
        oplog_on: bool,
        ns: &str,
        ui: Option<&[u8]>,
        oplog_entries: &mut Vec<OplogEntry>,
        pre_images: &mut Vec<Option<Vec<u8>>>,
    ) -> Result<()> {
        let opts = match coll_options(session, db, coll)? {
            Some(o) if o.get_bool("capped").unwrap_or(false) => o,
            _ => return Ok(()),
        };
        let num = |k: &str| -> Option<i64> {
            match opts.get(k) {
                Some(Bson::Int32(v)) => Some(*v as i64),
                Some(Bson::Int64(v)) => Some(*v),
                Some(Bson::Double(v)) => Some(*v as i64),
                _ => None,
            }
        };
        let size_limit = num("size");
        let max_limit = num("max");
        if size_limit.is_none() && max_limit.is_none() {
            return Ok(());
        }
        // `scan_docs` walks the doc table in RecordId (= insertion) order, so FIFO
        // eviction holds even for non-monotonic custom `_id`s.
        let scanned = self.scan_docs(session, db, coll)?;
        let mut total: i64 = scanned.iter().map(|(_, _, b)| b.len() as i64).sum();
        let mut count = scanned.len() as i64;
        let preimages_on = oplog_on && pre_post_images_enabled(session, db, coll)?;
        let doc_cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        for (recordid, id_k, blob) in scanned {
            let over_size = size_limit.is_some_and(|s| total > s);
            let over_max = max_limit.is_some_and(|m| count > m);
            if !over_size && !over_max {
                break;
            }
            if fresh_id_keys.contains(&id_k) {
                // Don't evict docs inserted in this batch — with monotonic
                // _ids they sort to the tail, so reaching one means the rest
                // are fresh too.
                break;
            }
            let doc = decode_doc(&blob)?;
            // Doc row first, entries after — see prune_ttl for the lock-free
            // reader ordering rationale.
            doc_cur.reset()?;
            doc_cur.set_key_ssq(db, coll, recordid);
            match doc_cur.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
            self.delete_index_entries(session, db, coll, &doc, descs, recordid)?;
            self.delete_nat_entry(session, db, coll, &id_k)?;
            total -= blob.len() as i64;
            count -= 1;
            if oplog_on {
                let id = doc.get("_id").cloned().unwrap_or(Bson::Null);
                // A delete records `{_id}` in both `o` and `o2` — encode it once.
                let id_doc = encode_id_doc(&id)?;
                oplog_entries.push(OplogEntry::Raw(Self::oplog_entry_crud(
                    "d", ns, ui, &id_doc, &id_doc,
                )?));
                pre_images.push(if preimages_on {
                    Some(encode_doc(&doc)?)
                } else {
                    None
                });
            }
        }
        Ok(())
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
    /// Every packed entry key `doc` contributes to `desc` (2d cell, 2dsphere
    /// covering cells, or the regular sparse/partial-gated key variants). The
    /// single source of truth for write / delete / diff maintenance — writing
    /// and retracting MUST enumerate identically or entries leak.
    fn packed_entry_keys(
        &self,
        doc: &Document,
        desc: &IndexDesc,
        recordid: i64,
    ) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        // 2d geo index: one cell entry per point-valued field (point-only).
        if let Some(geo) = &desc.geo_2d {
            if let Some(kb) = get_path(doc, &geo.field).and_then(|v| geo.cell_kb(v)) {
                out.push(pack_entry(&kb, recordid));
            }
            return Ok(out);
        }
        // 2dsphere S2 index: covering cells + ancestors per geometry field.
        if let Some(gs) = &desc.geo_sphere {
            if let Some(v) = get_path(doc, &gs.field) {
                for kb in gs.cell_kbs(v) {
                    out.push(pack_entry(&kb, recordid));
                }
            }
            return Ok(out);
        }
        if !self.doc_in_partial(doc, desc)? {
            return Ok(out);
        }
        for kb in index_key_variants(doc, &desc.key_spec, desc.sparse)? {
            out.push(pack_entry(&kb, recordid));
        }
        Ok(out)
    }

    fn write_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        doc: &Document,
        descs: &[IndexDesc],
        recordid: i64,
    ) -> Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        // The entry's trailing half is the doc's RecordId (step 2), so the old
        // `id_key_override` plumbing that existed purely to compute it is gone.
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            for packed in self.packed_entry_keys(doc, desc, recordid)? {
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
        recordid: i64,
    ) -> Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            for packed in self.packed_entry_keys(doc, desc, recordid)? {
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

    /// Index maintenance for an update: the set difference between the old and
    /// new doc's packed entry keys, per index. Unchanged keys get NO WT
    /// operations — which is both the fast path (a `$set` of an unindexed
    /// field touches no entries at all) and the correctness path for
    /// lock-free readers: the old delete-everything-then-rewrite scheme
    /// opened a window where a doc vanished from an index whose value the
    /// update never changed. Callers insert `additions` before or right after
    /// the doc-row write and remove `removals` last, so an interleaving
    /// reader only ever sees a superset (deduped and re-verified by the
    /// matcher), never a missing entry for a committed doc.
    fn index_entry_diff(
        &self,
        old_doc: &Document,
        new_doc: &Document,
        descs: &[IndexDesc],
        recordid: i64,
    ) -> Result<(EntryOps, EntryOps)> {
        let mut additions = Vec::new();
        let mut removals = Vec::new();
        if descs.is_empty() {
            return Ok((additions, removals));
        }
        // `_id` is immutable, so an update keeps the SAME RecordId — one value
        // serves both sides of the diff.
        for desc in descs {
            let old_keys: HashSet<Vec<u8>> = self
                .packed_entry_keys(old_doc, desc, recordid)?
                .into_iter()
                .collect();
            let new_keys: HashSet<Vec<u8>> = self
                .packed_entry_keys(new_doc, desc, recordid)?
                .into_iter()
                .collect();
            for packed in new_keys.difference(&old_keys) {
                additions.push((desc.name.clone(), packed.clone()));
            }
            for packed in old_keys.difference(&new_keys) {
                removals.push((desc.name.clone(), packed.clone()));
            }
        }
        Ok((additions, removals))
    }

    fn insert_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        items: &[(String, Vec<u8>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (name, packed) in items {
            cur.reset()?;
            cur.set_key_sssu(db, coll, name, packed);
            cur.set_value_u(b"");
            cur.insert()?;
        }
        Ok(())
    }

    fn remove_index_entries(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        items: &[(String, Vec<u8>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for (name, packed) in items {
            cur.reset()?;
            cur.set_key_sssu(db, coll, name, packed);
            match cur.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
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
                let geo_sphere = parse_geo_sphere(&key_spec);
                IndexDesc {
                    name,
                    key_spec,
                    sparse: opts.get_bool("sparse").unwrap_or(false),
                    unique: opts.get_bool("unique").unwrap_or(false),
                    prepare_unique: opts.get_bool("prepareUnique").unwrap_or(false),
                    partial,
                    geo_2d,
                    geo_sphere,
                }
            })
            .collect())
    }

    /// The first unique-index violation `candidate` would cause, or `None`.
    /// Probes the entries table for an existing row sharing *any* key the
    /// candidate generates and belonging to a *different* doc (`exclude_recordid`
    /// skips the candidate's own row, for replace/update). Every key, not just
    /// the canonical one: on a multikey index mongod's rule is "no two docs
    /// share a generated key", and for a path descending through an array the
    /// canonical key isn't among the entries at all.
    /// Mirrors `storage._unique_conflict`.
    fn unique_conflict(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        candidate: &Document,
        descs: &[IndexDesc],
        exclude_recordid: Option<i64>,
    ) -> Result<Option<UniqueConflict>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        for desc in descs {
            // `prepareUnique` arms an index to reject dup *new* writes (11000)
            // before it's formally unique — so it enforces uniqueness here too.
            if (!desc.unique && !desc.prepare_unique) || !self.doc_in_partial(candidate, desc)? {
                continue;
            }
            for kb in index_key_variants(candidate, &desc.key_spec, desc.sparse)? {
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
                    // `row_id` is None only for a step-1-format entry, which a
                    // step-2 store cannot contain (refused at open) — treat it as
                    // "not me" rather than silently matching.
                    let is_self = row_id.is_some() && exclude_recordid == row_id;
                    if !is_self {
                        return Ok(Some(UniqueConflict {
                            index: desc.name.clone(),
                            key_pattern: desc.key_spec.clone(),
                            key_value: conflict_key_value(candidate, &desc.key_spec, &kb),
                        }));
                    }
                    more = cur.next()?;
                }
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

    /// Collection names registered under `db` (no lock, session-scoped — for
    /// `drop_database`). Unlike the public `list_collections` this omits the
    /// synthetic `local.oplog.rs` view and stays unsorted.
    fn colls_of(&self, session: &Session, db: &str) -> Result<Vec<String>> {
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

    /// Delete a collection's document / index-registry / index-entry rows
    /// (everything except its `secantus_collections` registry row). Shared by
    /// `drop_collection` / `drop_database` / `rename_collection`.
    fn purge_collection_tables(&self, session: &Session, db: &str, coll: &str) -> Result<()> {
        let doc_cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        for (recordid, _id_k, _blob) in self.scan_docs(session, db, coll)? {
            doc_cur.reset()?;
            doc_cur.set_key_ssq(db, coll, recordid);
            match doc_cur.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        for (name, _key_spec, _opts) in self.iter_indexes(session, db, coll)? {
            self.delete_entries_prefix(session, db, coll, &name)?;
            let ic = session.open_cursor(IDX_TABLE, None)?;
            ic.set_key_sss(db, coll, &name);
            match ic.remove() {
                Ok(()) => {}
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Drop the natural-order entries too, so a re-created collection can't
        // resurrect stale insertion positions (which would double a re-inserted
        // `_id` in a natural scan).
        self.drop_nat_collection(session, db, coll)?;
        Ok(())
    }

    /// Raw `(name, payload_bytes)` index-registry rows for `(db, coll)`, for the
    /// verbatim re-key in `rename_collection`.
    fn collect_idx_rows(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
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
            out.push((name, cur.get_value_u()?));
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Raw `(name, packed)` index-entry rows for `(db, coll)`, for the verbatim
    /// re-key in `rename_collection`.
    fn collect_entry_rows(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let cur = session.open_cursor(IDX_ENTRIES_TABLE, None)?;
        let mut out = Vec::new();
        cur.set_key_sssu(db, coll, "", b"");
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
            let (d, c, name, packed) = cur.get_key_sssu()?;
            if d != db || c != coll {
                break;
            }
            out.push((name, packed));
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Introspection: the entries of index `name` as `(escaped_kb, id_key)`
    /// pairs in WiredTiger (sorted) order. Primarily for tests and explain-style
    /// inspection; the lookup paths in later slices read entries directly.
    pub fn index_entries(&self, db: &str, coll: &str, name: &str) -> Result<Vec<(Vec<u8>, i64)>> {
        // Lock-free read (see the `lock` field's invariants).
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
            if let Some(rid) = idk {
                out.push((kb.to_vec(), rid));
            }
            more = scan.next()?;
        }
        Ok(out)
    }

    // --- query routing (Phase 4 sub-phase 2) ---

    /// Documents matching `filter`, as BSON bytes, in `_id`-natural / index order.
    /// Convenience wrapper for `find_matching_with(.., None, None)`.
    pub fn find_matching(&self, db: &str, coll: &str, filter: &Document) -> Result<Vec<Vec<u8>>> {
        self.find_matching_with(db, coll, filter, None, None, None, &Document::new())
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
    #[allow(clippy::too_many_arguments)]
    pub fn find_matching_with(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        sort: Option<&Document>,
        hint: Option<&Hint>,
        coll_opt: Option<&Collation>,
        vars: &Document,
    ) -> Result<Vec<Vec<u8>>> {
        if self.is_oplog_rs(db, coll) {
            return self.find_oplog_rs(filter, sort, coll_opt, vars);
        }
        // Lock-free read (see the `lock` field's invariants).
        let session = self.op_session()?;
        let (sort_field, sort_dir) = single_sort_spec(sort);
        let mut in_sort_order = false;
        // A collation makes the byte-sortable indexes (collation-naive) unsafe for
        // both filtering and sort-order, so force a collection scan + in-memory
        // collation-aware match/sort. (Per-index-collation IXSCAN is a later
        // optimisation.)
        let force_collscan = coll_opt.is_some();

        let blobs: Vec<Vec<u8>> = if force_collscan {
            self.scan_blobs_natural(&session, db, coll)?
        } else if let Some(h) = hint {
            let resolved = self.resolve_hint(&session, db, coll, h)?;
            let (cands, ord) =
                self.candidates_from_hint(&session, db, coll, &resolved, sort_field, sort_dir)?;
            in_sort_order = ord;
            cands
        } else if let Some(id_keys) = self.try_index_id_keys(&session, db, coll, filter)? {
            let mut docs = self.docs_by_recordids(&session, db, coll, &id_keys)?;
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
                    None => self.scan_blobs_natural(&session, db, coll)?,
                }
            } else if let Some(multi) = multi_sort_spec(sort).filter(|m| m.len() > 1) {
                // Multi-field sort: walk a strict-match compound index, else COLLSCAN.
                match self.compound_index_for_sort(&session, db, coll, &multi)? {
                    Some((idx_name, reverse)) => {
                        in_sort_order = true;
                        self.walk_index_in_order(&session, db, coll, &idx_name, reverse)?
                    }
                    None => self.scan_blobs_natural(&session, db, coll)?,
                }
            } else {
                self.scan_blobs_natural(&session, db, coll)?
            }
        } else {
            self.scan_blobs_natural(&session, db, coll)?
        };

        // Filter over RAW BSON: `matches_raw` decodes only the fields the filter
        // reaches, so a selective filter over wide documents never materialises
        // the sibling fields (Finding 1, tasks/rust-perf-findings.md). Unmatched
        // documents are never fully decoded; a no-sort find never decodes at all.
        // `vars` carries command `let` bindings for `$expr` in the filter.
        //
        // An empty filter (`find({})`) matches every document, so skip the per-doc
        // raw-match entirely — the whole `blobs` vector is the result set (a full
        // scan of N documents would otherwise pay N `RawDocument::from_bytes` +
        // `matches_raw` calls for a foregone `true`).
        let out: Vec<Vec<u8>> = if filter.is_empty() {
            blobs
        } else {
            let mut out = Vec::new();
            for blob in blobs {
                let raw = bson::RawDocument::from_bytes(&blob)
                    .map_err(|_| StorageError::QueryUnsupported)?;
                if secantus_core::query::matches_raw(raw, filter, vars, coll_opt)
                    .map_err(|_| StorageError::QueryUnsupported)?
                {
                    out.push(blob);
                }
            }
            out
        };
        if !in_sort_order {
            if let Some(spec) = multi_sort_spec(sort) {
                // A post-sort needs the sort-field values, so decode the *matched*
                // documents only (the filter already discarded the rest). Decorate
                // -sort-undecorate on the byte-sortable compound key (collation-
                // folded when a collation is active).
                let mut keyed: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(out.len());
                for blob in out {
                    let d = decode_doc(&blob)?;
                    keyed.push((sort_key(&d, &spec, coll_opt)?, blob));
                }
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                return Ok(keyed.into_iter().map(|(_, b)| b).collect());
            }
        }
        Ok(out)
    }

    /// Candidate `(id_key, blob)` pairs for `filter`: index-routed (deduped) when
    /// the filter is covered by an index, else a full natural-order collection
    /// scan. Always fully materialised — the doc-table writes that
    /// `update_matching` / `delete_matching` perform during the loop would
    /// invalidate a still-walking scan cursor on the same session.
    fn candidate_docs(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
        force_scan: bool,
    ) -> Result<Vec<ScannedDoc>> {
        // `force_scan` (a collation is active) bypasses the collation-naive
        // indexes for a full collection scan + in-memory collation matching.
        if filter.is_empty() || force_scan {
            return self.scan_docs(session, db, coll);
        }
        if let Some(recordids) = self.try_index_id_keys(session, db, coll, filter)? {
            // Index entries carry the RecordId (step 2), so this reads the doc row
            // directly — no `id_key -> _id index -> RecordId` hop. The doc's own
            // id_key comes back from the framed value, which is what the callers
            // (delete / update maintenance) need it for.
            let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
            let mut seen: HashSet<i64> = HashSet::new();
            let mut out = Vec::new();
            for recordid in recordids {
                if !seen.insert(recordid) {
                    continue;
                }
                cur.reset()?;
                cur.set_key_ssq(db, coll, recordid);
                match cur.search() {
                    Ok(()) => {
                        let value = cur.get_value_u()?;
                        let (idk, blob) = unframe_doc_value(&value)?;
                        out.push((recordid, idk.to_vec(), blob.to_vec()));
                    }
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(out)
        } else {
            self.scan_docs(session, db, coll)
        }
    }

    /// Count documents matching `filter` (the whole collection when empty).
    /// Mirrors `storage.count_matching` (base form — `let` / `collation` route
    /// to Python at the engine-selection layer).
    pub fn count_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        coll_opt: Option<&Collation>,
    ) -> Result<usize> {
        if self.is_oplog_rs(db, coll) {
            return Ok(self
                .find_oplog_rs(filter, None, coll_opt, &Document::new())?
                .len());
        }
        // Lock-free read (see the `lock` field's invariants).
        let session = self.op_session()?;
        if filter.is_empty() {
            return Ok(self.scan_docs(&session, db, coll)?.len());
        }
        let vars = Document::new();
        let mut n = 0usize;
        for (_rid, _id_k, blob) in
            self.candidate_docs(&session, db, coll, filter, coll_opt.is_some())?
        {
            // Match over raw BSON — count never returns the documents, so a
            // selective filter over wide documents decodes only the filter's
            // fields, nothing else (matches `find_matching_with`).
            let raw =
                bson::RawDocument::from_bytes(&blob).map_err(|_| StorageError::QueryUnsupported)?;
            if secantus_core::query::matches_raw(raw, filter, &vars, coll_opt)
                .map_err(|_| StorageError::QueryUnsupported)?
            {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Apply `update` to documents matching `filter`. `multi` updates every
    /// match (else just the first); `upsert` inserts a seeded document when
    /// nothing matches. Operator-style updates emit a `$v:2` diff oplog entry,
    /// replacement-style updates emit the full new doc; index entries, multikey
    /// flags, unique enforcement and pre-images are all maintained. Mirrors
    /// `storage.update_matching` (base form — `array_filters` / positional
    /// operators / `let` / `collation` / `validator` / capped collections route
    /// to Python at the engine-selection layer).
    #[allow(clippy::too_many_arguments)]
    pub fn update_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        update: &Document,
        multi: bool,
        upsert: bool,
        array_filters: &[Document],
        let_vars: &Document,
        coll_opt: Option<&Collation>,
        validator: Option<&Document>,
    ) -> Result<UpdateOutcome> {
        // Operator-/replacement-form update. `is_replacement` (no `$`-prefixed
        // top-level key) drives the oplog shape: a replacement emits the whole
        // doc in `o`, an operator update a `{$v:2, diff}`. Positional operators
        // resolve per matched doc — `$` from the query filter
        // (`find_positional_matches`), `$[]`/`$[ident]` from `array_filters`.
        // `let_vars` are visible to `$expr` in the filter (command `let`);
        // `coll_opt` forces a collation-aware COLLSCAN match.
        let is_replacement = !update.keys().any(|k| k.starts_with('$'));
        // Resolve `$currentDate` to a concrete clock value once per operation (so
        // a multi-update stamps every matched doc with the same time), keeping the
        // deterministic core engine free of the clock.
        let update = resolve_current_date(update)?;
        let update = &update;
        self.update_matching_core(
            db,
            coll,
            filter,
            let_vars,
            coll_opt,
            multi,
            upsert,
            is_replacement,
            validator,
            &|doc, up| {
                // mongod rejects any update that would change the immutable `_id`
                // with ImmutableField (66) — surface that specific code rather than
                // a generic "unsupported".
                if let Some(old_id) = doc.get("_id") {
                    if update_would_change_id(update, old_id) {
                        return Err(StorageError::ImmutableField);
                    }
                }
                let pos = secantus_core::update::find_positional_matches(doc, filter);
                secantus_core::update::apply_update_with(doc, update, up, array_filters, &pos)
                    .map_err(|_| StorageError::QueryUnsupported)
            },
        )
    }

    /// Pipeline-form update (`u: [ {$set: …}, … ]`). Each matched doc is rewritten
    /// by running the aggregation pipeline over it (`apply_pipeline` on the single
    /// doc); the original `_id` is re-applied when a stage (`$replaceRoot` /
    /// `$replaceWith` / `$project`) drops it — mongod keeps `_id` immutable across
    /// an update pipeline. Always diff-style in the
    /// oplog (`is_replacement = false`) so change streams report
    /// `operationType: "update"` — mirrors `storage.update_matching`'s list branch.
    /// On upsert with no match, the pipeline runs over the filter-seeded doc.
    #[allow(clippy::too_many_arguments)]
    pub fn update_matching_pipeline(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        pipeline: &[Bson],
        multi: bool,
        upsert: bool,
        let_vars: &Document,
        coll_opt: Option<&Collation>,
        validator: Option<&Document>,
    ) -> Result<UpdateOutcome> {
        self.update_matching_core(
            db,
            coll,
            filter,
            let_vars,
            coll_opt,
            multi,
            upsert,
            false,
            validator,
            &|doc, _up| {
                let out = secantus_core::aggregate::apply_pipeline(
                    vec![doc.clone()],
                    pipeline,
                    let_vars,
                    None,
                )
                .map_err(|_| StorageError::QueryUnsupported)?;
                let mut new = out
                    .into_iter()
                    .next()
                    .ok_or(StorageError::QueryUnsupported)?;
                // mongod preserves the original `_id` through an update pipeline —
                // a `$replaceRoot`/`$replaceWith`/`$project` stage can drop it, but
                // the stored doc keeps its `_id` (which is immutable). Re-add it if
                // a stage removed it. (`encode_doc` then restores `_id`-first order.)
                if !new.contains_key("_id") {
                    if let Some(id) = doc.get("_id") {
                        new.insert("_id", id.clone());
                    }
                }
                Ok(new)
            },
        )
    }

    /// Shared match → rewrite → write → oplog/index path for both the
    /// operator/replacement form and the pipeline form. `transform(doc, is_upsert)`
    /// produces the new doc; `is_replacement` selects the oplog `o` shape.
    #[allow(clippy::too_many_arguments)]
    fn update_matching_core(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        vars: &Document,
        coll_opt: Option<&Collation>,
        multi: bool,
        upsert: bool,
        is_replacement: bool,
        validator: Option<&Document>,
        transform: &dyn Fn(&Document, bool) -> Result<Document>,
    ) -> Result<UpdateOutcome> {
        self.retry_write_conflicts("update_matching_core", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                ensure_collection(&session, db, coll)?;
                let ns = format!("{db}.{coll}");
                let descs = self.index_descs(&session, db, coll)?;
                let oplog_on = self.enable_oplog;
                let preimages_on = oplog_on && pre_post_images_enabled(&session, db, coll)?;
                let ui = if oplog_on {
                    Some(collection_uuid(&session, db, coll)?)
                } else {
                    None
                };

                let mut matched = 0usize;
                let mut modified = 0usize;
                let mut post_image: Option<Document> = None;
                let mut oplog_entries: Vec<OplogEntry> = Vec::new();
                let mut pre_images: Vec<Option<Vec<u8>>> = Vec::new();

                let candidates =
                    self.candidate_docs(&session, db, coll, filter, coll_opt.is_some())?;
                for (recordid, id_k, blob) in candidates {
                    // Match over raw BSON first (decodes only the filter's fields);
                    // a rejected candidate skips the full document decode. A
                    // matched candidate is then decoded for the update transform.
                    let raw = bson::RawDocument::from_bytes(&blob)
                        .map_err(|_| StorageError::QueryUnsupported)?;
                    if !secantus_core::query::matches_raw(raw, filter, vars, coll_opt)
                        .map_err(|_| StorageError::QueryUnsupported)?
                    {
                        continue;
                    }
                    let doc = decode_doc(&blob)?;
                    matched += 1;
                    let new = transform(&doc, false)?;
                    if !multi {
                        // Captured before the oplog branch below moves `new`; the
                        // post-image is the applied doc even when the update was a
                        // no-op (`new == doc`), matching mongod's fam reply.
                        post_image = Some(new.clone());
                    }
                    if new != doc {
                        // Collection validator on the post-apply doc (mongod rejects an
                        // update that would leave a document failing validation). A
                        // validator the query engine can't evaluate is treated as
                        // passing (lenient), matching the insert path.
                        if let Some(v) = validator {
                            if !query_matches(&new, v, &Document::new(), None).unwrap_or(true) {
                                return Err(StorageError::DocumentValidationFailure);
                            }
                        }
                        if let Some(c) =
                            self.unique_conflict(&session, db, coll, &new, &descs, Some(recordid))?
                        {
                            return Err(StorageError::DuplicateKey(Box::new(c)));
                        }
                        let new_blob = encode_doc(&new)?;
                        if new_blob.len() > MAX_BSON_OBJECT_SIZE {
                            return Err(StorageError::DocumentTooLarge(new_blob.len()));
                        }
                        modified += 1;
                        // The doc stays at its existing key (`id_k`, suffixed for
                        // timeseries). Entry maintenance is a set diff: additions land
                        // before the doc-row write and removals after, so a lock-free
                        // reader interleaving here sees at worst a superset (see
                        // `index_entry_diff`), never a missing entry for this doc.
                        let (additions, removals) =
                            self.index_entry_diff(&doc, &new, &descs, recordid)?;
                        self.insert_index_entries(&session, db, coll, &additions)?;
                        // The doc stays at its RecordId (unchanged — `_id` is immutable);
                        // the framed value carries the id_key in-band.
                        let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                        cur.set_key_ssq(db, coll, recordid);
                        cur.set_value_u(&frame_doc_value(&id_k, &new_blob));
                        cur.update()?;
                        self.remove_index_entries(&session, db, coll, &removals)?;
                        self.maybe_mark_multikey(&session, db, coll, &new, &descs)?;
                        if oplog_on {
                            // Replacement `o` is the whole new doc — reuse the
                            // `new_blob` we already encoded for the doc-table write
                            // (no second serialize). Operator `o` is the small
                            // `{$v:2, diff}`, encoded fresh.
                            let o_owned: Vec<u8>;
                            let o_bytes: &[u8] = if is_replacement {
                                &new_blob
                            } else {
                                let mut o = Document::new();
                                o.insert("$v", 2i32);
                                o.insert(
                                    "diff",
                                    Bson::Document(
                                        compute_update_description(&doc, &new)
                                            .map_err(|_| StorageError::QueryUnsupported)?,
                                    ),
                                );
                                o_owned = encode_doc(&o)?;
                                &o_owned
                            };
                            let o2 = encode_id_doc(&doc.get("_id").cloned().unwrap_or(Bson::Null))?;
                            oplog_entries.push(OplogEntry::Raw(Self::oplog_entry_crud(
                                "u",
                                &ns,
                                Some(ui.as_ref().unwrap()),
                                o_bytes,
                                &o2,
                            )?));
                            pre_images.push(if preimages_on {
                                Some(encode_doc(&doc)?)
                            } else {
                                None
                            });
                        }
                    }
                    if !multi {
                        break;
                    }
                }

                let mut upserted_id: Option<Bson> = None;
                if matched == 0 && upsert {
                    // Seed from the filter's bare-equality fields, then apply the update.
                    // A document value is skipped only when it's an OPERATOR expression
                    // (`{$gt: 5}`); a literal subdocument equality (`{f: .., f2: ..}`,
                    // e.g. a compound `_id`) is a real predicate and must be seeded —
                    // dropping it would mint a fresh ObjectId instead of using it.
                    let mut seed = Document::new();
                    for (k, v) in filter {
                        if !k.starts_with('$') && !is_op_doc(v) {
                            seed.insert(k.clone(), v.clone());
                        }
                    }
                    let mut new = transform(&seed, true)?;
                    if !new.contains_key("_id") {
                        new.insert("_id", Bson::ObjectId(ObjectId::new()));
                    }
                    // Validator on an upsert-inserted document, too.
                    if let Some(v) = validator {
                        if !query_matches(&new, v, &Document::new(), None).unwrap_or(true) {
                            return Err(StorageError::DocumentValidationFailure);
                        }
                    }
                    let id = new.get("_id").cloned().unwrap();
                    if let Some(c) = self.unique_conflict(&session, db, coll, &new, &descs, None)? {
                        return Err(StorageError::DuplicateKey(Box::new(c)));
                    }
                    let mut new_id_key = id_key(&id)?;
                    // Timeseries: suffix the upserted doc's key so duplicate `_id`s coexist.
                    if self.is_timeseries(&session, db, coll)? {
                        new_id_key.extend_from_slice(&self.timeseries_doc_suffix());
                    }
                    let new_blob = encode_doc(&new)?;
                    if new_blob.len() > MAX_BSON_OBJECT_SIZE {
                        return Err(StorageError::DocumentTooLarge(new_blob.len()));
                    }
                    // Mint the RecordId + write the `_id` index first, then key the
                    // doc row by that RecordId (framed value carries the id_key).
                    let recordid = self.write_nat_entry(&session, db, coll, &new_id_key)?;
                    let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                    cur.set_key_ssq(db, coll, recordid);
                    cur.set_value_u(&frame_doc_value(&new_id_key, &new_blob));
                    cur.insert()?;
                    self.write_index_entries(&session, db, coll, &new, &descs, recordid)?;
                    self.maybe_mark_multikey(&session, db, coll, &new, &descs)?;
                    post_image = Some(new.clone());
                    if oplog_on {
                        // The upserted doc is recorded as an insert; splice the
                        // `new_blob` we already encoded for the doc-table write.
                        let o2 = encode_id_doc(&id)?;
                        oplog_entries.push(OplogEntry::Raw(Self::oplog_entry_crud(
                            "i",
                            &ns,
                            Some(ui.as_ref().unwrap()),
                            &new_blob,
                            &o2,
                        )?));
                        pre_images.push(None);
                    }
                    upserted_id = Some(id);
                }

                if oplog_on && !oplog_entries.is_empty() {
                    self.emit_oplog_entries(&session, oplog_entries, pre_images)?;
                }
                Ok(UpdateOutcome {
                    matched,
                    modified,
                    upserted_id,
                    post_image,
                })
            })
        })
    }

    /// Delete documents matching `filter`, returning how many were removed.
    /// `limit > 0` caps the number deleted (1 for `deleteOne`; 0 = all matches).
    /// Maintains index entries and emits op `"d"` oplog entries (+ pre-images
    /// when enabled). Mirrors `storage.delete_matching` (base form — `let` /
    /// `collation` route to Python at the engine-selection layer).
    pub fn delete_matching(
        &self,
        db: &str,
        coll: &str,
        filter: &Document,
        limit: usize,
        let_vars: &Document,
        coll_opt: Option<&Collation>,
    ) -> Result<usize> {
        self.retry_write_conflicts("delete_matching", || {
            let lock = self.coll_lock(db, coll);
            let _c = lock.lock().unwrap_or_else(|e| e.into_inner());
            let session = self.op_session()?;
            self.with_statement_txn(&session, || {
                let descs = self.index_descs(&session, db, coll)?;
                let oplog_on = self.enable_oplog;
                let preimages_on = oplog_on && pre_post_images_enabled(&session, db, coll)?;
                let ui = if oplog_on && coll_options(&session, db, coll)?.is_some() {
                    Some(collection_uuid(&session, db, coll)?)
                } else {
                    None
                };
                let ns = if oplog_on {
                    format!("{db}.{coll}")
                } else {
                    String::new()
                };

                let mut deleted = 0usize;
                let mut oplog_entries: Vec<OplogEntry> = Vec::new();
                let mut pre_images: Vec<Option<Vec<u8>>> = Vec::new();
                let candidates =
                    self.candidate_docs(&session, db, coll, filter, coll_opt.is_some())?;
                for (recordid, id_k, blob) in candidates {
                    // Match over raw BSON first (decodes only the filter's fields);
                    // a rejected candidate skips the full decode. A matched
                    // candidate is decoded for the delete's oplog `o2` / pre-image.
                    let raw = bson::RawDocument::from_bytes(&blob)
                        .map_err(|_| StorageError::QueryUnsupported)?;
                    if !secantus_core::query::matches_raw(raw, filter, let_vars, coll_opt)
                        .map_err(|_| StorageError::QueryUnsupported)?
                    {
                        continue;
                    }
                    let doc = decode_doc(&blob)?;
                    // Doc row first, entries after — see prune_ttl for the lock-free
                    // reader ordering rationale.
                    let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
                    cur.set_key_ssq(db, coll, recordid);
                    cur.remove()?;
                    self.delete_index_entries(&session, db, coll, &doc, &descs, recordid)?;
                    self.delete_nat_entry(&session, db, coll, &id_k)?;
                    deleted += 1;
                    if oplog_on {
                        // `o` and `o2` are both `{_id}` — encode once, splice twice.
                        let id_doc = encode_id_doc(&doc.get("_id").cloned().unwrap_or(Bson::Null))?;
                        oplog_entries.push(OplogEntry::Raw(Self::oplog_entry_crud(
                            "d",
                            &ns,
                            ui.as_deref(),
                            &id_doc,
                            &id_doc,
                        )?));
                        pre_images.push(if preimages_on {
                            Some(encode_doc(&doc)?)
                        } else {
                            None
                        });
                    }
                    if limit > 0 && deleted >= limit {
                        break;
                    }
                }
                if oplog_on && !oplog_entries.is_empty() {
                    self.emit_oplog_entries(&session, oplog_entries, pre_images)?;
                }
                Ok(deleted)
            })
        })
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
        // Lock-free read (see the `lock` field's invariants).
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

    /// Raw doc blobs for `(db, coll)` in natural (insertion / RecordId) order.
    fn scan_blobs(&self, session: &Session, db: &str, coll: &str) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .scan_docs(session, db, coll)?
            .into_iter()
            .map(|(_rid, _id_k, blob)| blob)
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
            // `$natural` is insertion order — walk the natural-order index.
            ResolvedHint::Natural => Ok((self.scan_blobs_natural(session, db, coll)?, false)),
            ResolvedHint::IdIndex => {
                // The doc table is keyed by id_key, so this scan IS _id order.
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
        let mut recordids: Vec<i64> = Vec::new();
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
            if let Some(rid) = row_id {
                recordids.push(rid);
            }
            more = cur.next()?;
        }
        if reverse {
            recordids.reverse();
        }
        self.docs_by_recordids(session, db, coll, &recordids)
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
    ) -> Result<Option<Vec<i64>>> {
        if filter.is_empty() {
            return Ok(None);
        }
        if filter.keys().any(|f| f.starts_with('$')) {
            return Ok(None);
        }
        // `_id` equality is a primary-key point lookup on the documents table —
        // EXCEPT in a timeseries collection, where the doc-table key is suffixed
        // (duplicate `_id`s coexist), so a reconstructed bare key would miss
        // rows. There, fall through to a collection scan.
        if filter.len() == 1 && !self.is_timeseries(session, db, coll)? {
            if let Some(spec) = filter.get("_id") {
                if let Some(id_keys) = id_point_lookup_keys(spec)? {
                    // Callers want RecordIds. For an `_id` lookup the `_id` index IS
                    // the primary access path (not the secondary-index hop step 2
                    // removed), so resolve each key through it; a key with no row
                    // simply matches nothing.
                    let mut rids = Vec::with_capacity(id_keys.len());
                    for id_k in &id_keys {
                        if let Some(rid) = self.doc_recordid(session, db, coll, id_k)? {
                            rids.push(rid);
                        }
                    }
                    return Ok(Some(rids));
                }
            }
        }
        // Geo dispatch: a `$geoWithin` on a 2d-indexed field scans the geohash
        // covering range; on a 2dsphere field, the S2 cell covering.
        if let Some(r) = self.try_geo_2d_id_keys(session, db, coll, filter)? {
            return Ok(Some(r));
        }
        if let Some(r) = self.try_geo_sphere_id_keys(session, db, coll, filter)? {
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
        if filter.len() == 1 {
            let (field, value) = filter.iter().next().unwrap();
            let idx = match self.find_leading_field_index(session, db, coll, field, filter)? {
                Some(m) => m,
                None => return Ok(None),
            };
            return self.lookup_id_keys_via_leading_field(session, db, coll, &idx, value);
        }
        // Multi-field filter: a single-field index can still serve it when every
        // other filter field is absorbed by the index's (implied) partial filter.
        let (_field, value, idx_match) =
            match self.single_field_partial_residual_match(session, db, coll, filter)? {
                Some(m) => m,
                None => return Ok(None),
            };
        self.lookup_id_keys_via_leading_field(session, db, coll, &idx_match, &value)
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
    ) -> Result<Option<Vec<i64>>> {
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

    /// Find the `2dsphere` index covering `field`, as `(index_name, params)`.
    fn geo_sphere_for(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        field: &str,
    ) -> Result<Option<(String, GeoSphere)>> {
        for desc in self.index_descs(session, db, coll)? {
            if let Some(g) = &desc.geo_sphere {
                if g.field == field {
                    return Ok(Some((desc.name.clone(), g.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Candidate `id_key`s for `{field: {$geoWithin: <region>}}` via a `2dsphere`
    /// index: S2-cover the query region's bounding box (cells + ancestors) and
    /// do an exact point-lookup per cell against the entries table, unioning the
    /// hits (a superset — `find_matching` re-checks each with `matches()`).
    /// `None` (→ COLLSCAN) if there's no 2dsphere index on `field`, the filter
    /// isn't a lone `$geoWithin`, or the region's matching itself defers.
    fn try_geo_sphere_id_keys(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<Vec<i64>>> {
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
        let (name, _gs) = match self.geo_sphere_for(session, db, coll, field)? {
            Some(x) => x,
            None => return Ok(None),
        };
        let (min_x, min_y, max_x, max_y) = match secantus_core::geo::query_within_bbox(within) {
            Some(b) => b,
            None => return Ok(None),
        };
        let mut out: Vec<i64> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        for cid in s2_cells_for_bbox(min_x, min_y, max_x, max_y) {
            let kb = secantus_core::geo::encode_cell(cid);
            for id_k in self.scan_index_for_id_keys(session, db, coll, &name, &kb, false)? {
                if seen.insert(id_k) {
                    out.push(id_k);
                }
            }
        }
        Ok(Some(out))
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

    /// The `partialFilterExpression` of index `name`, or `None` (non-partial /
    /// absent). Used by the residual-match path to verify residual fields are
    /// exactly partial-filter fields.
    fn partial_filter_for(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        name: &str,
    ) -> Result<Option<Document>> {
        for desc in self.index_descs(session, db, coll)? {
            if desc.name == name {
                return Ok(desc.partial.clone());
            }
        }
        Ok(None)
    }

    /// For a *multi-field* filter, find a single-field (or leading-field) index
    /// whose leading field serves one clause while every **other** filter field
    /// is absorbed by the index's (implied) partial filter. e.g.
    /// `find({x: {$gt: 1}, a: 1})` against an index on `x` partial on
    /// `{a: {$lte: 1.5}}`: `x`'s range rides the index, `a: 1` is partial-implied
    /// (rechecked by the exact `matches()` pass in `find_matching`). Returns
    /// `(field, value, idx_match)` or `None`. Conservative: only *partial*
    /// indexes qualify, and only when the residual fields are exactly
    /// partial-filter fields. Mirrors
    /// `storage._single_field_partial_residual_match`.
    fn single_field_partial_residual_match(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        filter: &Document,
    ) -> Result<Option<(String, Bson, LeadingFieldMatch)>> {
        for (field, value) in filter.iter() {
            // An operator-form clause must be range ops the index can serve;
            // otherwise this field can't be the leading-field clause.
            if let Bson::Document(opd) = value {
                if opd.is_empty() || !opd.keys().all(|k| RANGE_OPS.contains(&k.as_str())) {
                    continue;
                }
            }
            let idx_match = match self.find_leading_field_index(session, db, coll, field, filter)? {
                Some(m) => m,
                None => continue,
            };
            let pf = match self.partial_filter_for(session, db, coll, &idx_match.0)? {
                Some(pf) => pf,
                None => continue,
            };
            // Every residual field must be a partial-filter field.
            let residual_ok = filter
                .keys()
                .filter(|f| f.as_str() != field)
                .all(|f| pf.contains_key(f));
            if !residual_ok {
                continue;
            }
            return Ok(Some((field.clone(), value.clone(), idx_match)));
        }
        Ok(None)
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
    ) -> Result<Option<Vec<i64>>> {
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
            let mut seen: HashSet<i64> = HashSet::new();
            let mut out: Vec<i64> = Vec::new();
            for v in vals {
                if matches!(v, Bson::Document(_)) {
                    return Ok(None);
                }
                for id_k in self.eq_id_keys(session, db, coll, name, direction, is_compound, v)? {
                    if seen.insert(id_k) {
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
    ) -> Result<Vec<i64>> {
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
    ) -> Result<Vec<i64>> {
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
            if let Some(rid) = row_id {
                out.push(rid);
            }
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
    ) -> Result<Vec<i64>> {
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
            if let Some(rid) = row_id {
                out.push(rid);
            }
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
    ) -> Result<Vec<i64>> {
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
            if let Some(rid) = row_id {
                out.push(rid);
            }
            more = cur.next()?;
        }
        Ok(out)
    }

    /// Fetch documents by `id_key` (deduped, order-preserving — a multikey index
    /// can yield the same `id_key` more than once). Mirrors
    /// `storage._docs_by_id_keys`.
    fn docs_by_recordids(
        &self,
        session: &Session,
        db: &str,
        coll: &str,
        recordids: &[i64],
    ) -> Result<Vec<Vec<u8>>> {
        // Index entries carry the RecordId directly (step 2), so an IXSCAN fetch
        // reads the doc row straight away. This used to resolve `id_key -> _id
        // index -> RecordId` first; deleting that hop is the point of step 2
        // (it measured +14.7% on `find_indexed_range`).
        let cur = session.open_cursor(&doc_table_for(db, coll), None)?;
        let mut seen: HashSet<i64> = HashSet::new();
        let mut out = Vec::new();
        for &recordid in recordids {
            if !seen.insert(recordid) {
                continue;
            }
            cur.reset()?;
            cur.set_key_ssq(db, coll, recordid);
            match cur.search() {
                Ok(()) => {
                    let value = cur.get_value_u()?;
                    let (_idk, blob) = unframe_doc_value(&value)?;
                    out.push(blob.to_vec());
                }
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
        // Timeseries skips the `_id` point lookup (suffixed keys) — see
        // `try_index_id_keys`; mirror that here so `explain` reports COLLSCAN.
        if filter.len() == 1 && !self.is_timeseries(session, db, coll)? {
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
        // Geo: a `$geoWithin` served by a 2dsphere index.
        if self
            .try_geo_sphere_id_keys(session, db, coll, filter)?
            .is_some()
        {
            let (field, _) = filter.iter().next().unwrap();
            if let Some((name, _g)) = self.geo_sphere_for(session, db, coll, field)? {
                let mut kp = Document::new();
                kp.insert(field.clone(), "2dsphere");
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
        if filter.len() == 1 {
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
            return match self.key_spec_for(session, db, coll, &idx.0)? {
                Some(key_spec) => Ok(Some((idx.0, key_spec))),
                None => Ok(None),
            };
        }
        // Multi-field filter: a single-field index whose leading field serves one
        // clause while the rest are absorbed by its (implied) partial filter.
        let idx_match = match self.single_field_partial_residual_match(session, db, coll, filter)? {
            Some(m) => m.2,
            None => return Ok(None),
        };
        match self.key_spec_for(session, db, coll, &idx_match.0)? {
            Some(key_spec) => Ok(Some((idx_match.0, key_spec))),
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
    ) -> Result<Option<Vec<i64>>> {
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
    ) -> Result<Option<Vec<i64>>> {
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
            let mut seen: HashSet<i64> = HashSet::new();
            let mut out: Vec<i64> = Vec::new();
            for v in vals {
                if matches!(v, Bson::Document(_)) {
                    return Ok(None);
                }
                let inner = make_kb(v)?;
                for id_k in
                    self.scan_index_for_id_keys(session, db, coll, &name, &inner, use_prefix)?
                {
                    if seen.insert(id_k) {
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

    /// `doc_shard_hash` must be byte-for-byte identical to the Python
    /// `storage._doc_shard_hash` so a collection routes to the same documents
    /// shard in both servers (cross-server backup / PITR portability). Values
    /// computed by the Python FNV-1a over `db + b"\0" + coll`.
    #[test]
    fn doc_shard_hash_matches_python() {
        assert_eq!(doc_shard_hash("harness", "w1"), 9941274063389089977);
        assert_eq!(doc_shard_hash("harness", "w8"), 9941281759970487454);
        assert_eq!(doc_shard_hash("test", "users"), 16319205138020980013);
        assert_eq!(doc_shard_hash("", ""), 12638153115695167455);
    }

    /// `wt_config` with the engine defaults must produce the exact
    /// `DEFAULT_CONFIG` string so `Storage::open` behaviour is unchanged.
    #[test]
    fn wt_config_matches_default() {
        assert_eq!(wt_config("256M", 1000, false, "128MB"), DEFAULT_CONFIG);
    }

    /// `extract_key_format` pulls the format token out of a WT metadata line,
    /// regardless of what other clauses precede it.
    #[test]
    fn extract_key_format_parses_the_token() {
        assert_eq!(
            extract_key_format("key_format=SSq,value_format=u"),
            Some("SSq")
        );
        assert_eq!(
            extract_key_format("app_metadata=(x=1),key_format=SSu,value_format=u"),
            Some("SSu")
        );
        assert_eq!(extract_key_format("value_format=u"), None);
    }

    /// A store whose document shards were written before the RecordId keying
    /// change (keyed `SSu`, unframed values) must be REFUSED at open, not
    /// silently mis-read with `SSq` cursor ops. There is no in-place migration
    /// (pre-1.0 beta) — see `tasks/backlog.md` §7.8.
    /// A store whose index entries predate the RecordId entry format must be
    /// REFUSED at open. Unlike the doc-table change this is invisible to WT's
    /// `key_format`, so the catalog's `entryFormat` marker is the only signal —
    /// strip it and the store must be rejected rather than reading `id_key`
    /// bytes as RecordIds.
    #[test]
    fn open_rejects_legacy_index_entry_format() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("db");
        std::fs::create_dir_all(&home).unwrap();
        let home_s = home.to_str().unwrap();
        {
            let st = Storage::open(home_s).unwrap();
            st.insert(
                "app",
                "c",
                vec![encode_doc(&doc! {"_id": 1i32, "x": 7i32}).unwrap()],
                true,
            )
            .unwrap();
            st.create_index("app", "c", "x_1", &doc! {"x": 1i32}, &Document::new())
                .unwrap();
        }
        // Downgrade the catalog row to a step-1 store: same bytes, marker removed.
        {
            let conn = Connection::open(home_s, DEFAULT_CONFIG).unwrap();
            let sess = conn.open_session().unwrap();
            let c = sess.open_cursor(IDX_TABLE, None).unwrap();
            c.set_key_sss("app", "c", "x_1");
            c.search().unwrap();
            let mut d = decode_doc(&c.get_value_u().unwrap()).unwrap();
            let mut opts = d.get_document("options").cloned().unwrap_or_default();
            assert_eq!(
                opts.get_i32("entryFormat").ok(),
                Some(ENTRY_FORMAT_RECORDID)
            );
            opts.remove("entryFormat");
            d.insert("options", Bson::Document(opts));
            c.reset().unwrap();
            c.set_key_sss("app", "c", "x_1");
            c.set_value_u(&encode_doc(&d).unwrap());
            c.update().unwrap();
        }
        match Storage::open(home_s) {
            Err(StorageError::Internal(m)) => {
                assert!(m.contains("entryFormat"), "unexpected message: {m}")
            }
            Err(other) => panic!("expected Internal fatal, got: {other:?}"),
            Ok(_) => panic!("expected open to be refused for a step-1 index entry format"),
        }
    }

    #[test]
    fn open_rejects_pre_recordid_doc_shard() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("db");
        std::fs::create_dir_all(&home).unwrap();
        let home_s = home.to_str().unwrap();
        // Fabricate the old layout: one documents shard keyed `SSu` with an
        // unframed value, exactly what a pre-RecordId beta wrote. Same WT config
        // `Storage` uses, so the reopen fails on our format check, not a config
        // clash.
        {
            let conn = Connection::open(home_s, DEFAULT_CONFIG).unwrap();
            let sess = conn.open_session().unwrap();
            sess.create(&doc_shard_name(0), "key_format=SSu,value_format=u")
                .unwrap();
            let c = sess.open_cursor(&doc_shard_name(0), None).unwrap();
            c.set_key_ssu("app", "c", b"\x2bid-key");
            c.set_value_u(b"raw-unframed-blob");
            c.insert().unwrap();
        } // drop closes the fabrication connection before the reopen
        match Storage::open(home_s) {
            Err(StorageError::Internal(m)) => {
                assert!(m.contains("RecordId"), "unexpected message: {m}")
            }
            Err(other) => panic!("expected Internal fatal, got: {other:?}"),
            Ok(_) => panic!("expected open to be refused for an SSu doc shard"),
        }
    }

    /// `sync_on_commit=true` flips `transaction_sync=enabled` to `true`.
    #[test]
    fn wt_config_sync_on_commit() {
        let s = wt_config("1G", 200, true, "2GB");
        assert!(s.contains("cache_size=1G"));
        assert!(s.contains("session_max=200"));
        assert!(s.contains("log=(enabled=true,file_max=2GB,prealloc=false)"));
        assert!(s.contains("transaction_sync=(enabled=true,method=fsync)"));
    }

    /// `resolve_durable` must match Python `Storage.__init__`'s precedence
    /// exactly: `SECANTUS_FORCE_DURABLE` wins over everything, then an explicit
    /// override, then `!SECANTUS_TEST_FAST_STORAGE`. Pure over its inputs so it
    /// needs no process-env mutation (which would race parallel tests).
    #[test]
    fn resolve_durable_precedence() {
        // force_durable=true overrides everything, incl. an explicit Some(false).
        assert!(resolve_durable(Some(false), true, true));
        assert!(resolve_durable(None, true, true));
        // Explicit override wins when not forced.
        assert!(resolve_durable(Some(true), false, true));
        assert!(!resolve_durable(Some(false), false, false));
        // Env-driven default: durable unless fast-storage is set.
        assert!(resolve_durable(None, false, false));
        assert!(!resolve_durable(None, false, true));
    }

    /// A durable close (`Drop` with `durable=true`) checkpoints and the data
    /// survives a reopen. Uses the explicit `Some(true)` override so the result
    /// is independent of the ambient `SECANTUS_TEST_FAST_STORAGE` /
    /// `SECANTUS_FORCE_DURABLE` env.
    #[test]
    fn durable_close_roundtrips_data_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("db");
        std::fs::create_dir_all(&home).unwrap();
        {
            let s = Storage::open_with_config_durable(
                home.to_str().unwrap(),
                DEFAULT_CONFIG,
                Some(true),
            )
            .unwrap();
            assert!(s.durable);
            assert!(!s.in_memory);
            s.insert(
                "app",
                "c",
                vec![encode_doc(&doc! {"_id": 1i32, "v": 42i32}).unwrap()],
                true,
            )
            .unwrap();
        } // Drop here runs the close-time checkpoint.

        let reopened = Storage::open(home.to_str().unwrap()).unwrap();
        let got = reopened.find_matching("app", "c", &doc! {}).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(decode_doc(&got[0]).unwrap().get_i32("v").unwrap(), 42);
    }

    /// Fast mode (`durable=false`) skips the close checkpoint but — because the
    /// journal stays enabled — data is still recoverable via log replay on
    /// reopen. This is the Rust analogue of Python's fast test-storage mode.
    #[test]
    fn fast_close_still_recovers_via_log_replay() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("db");
        std::fs::create_dir_all(&home).unwrap();
        {
            let s = Storage::open_with_config_durable(
                home.to_str().unwrap(),
                DEFAULT_CONFIG,
                Some(false),
            )
            .unwrap();
            assert!(!s.durable);
            s.insert(
                "app",
                "c",
                vec![encode_doc(&doc! {"_id": 7i32, "v": 99i32}).unwrap()],
                true,
            )
            .unwrap();
        } // Drop here does NOT checkpoint (fast mode).

        let reopened = Storage::open(home.to_str().unwrap()).unwrap();
        let got = reopened.find_matching("app", "c", &doc! {}).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(decode_doc(&got[0]).unwrap().get_i32("v").unwrap(), 99);
    }

    /// Ascending sort-key bytes for a value (what an ASC single-field entry's
    /// `kb` is).
    fn ev(b: &Bson) -> Vec<u8> {
        sortkey::encode_value(b, None).unwrap()
    }

    #[test]
    fn create_archive_roundtrips_data_through_extract() {
        // WiredTiger-backed: needs the rust-storage test env (WT + libclang).
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("src");
        std::fs::create_dir_all(&home).unwrap();
        let s = Storage::open(home.to_str().unwrap()).unwrap();
        s.insert(
            "app",
            "c",
            vec![encode_doc(&doc! {"_id": 1i32, "v": 7i32}).unwrap()],
            true,
        )
        .unwrap();

        let archive = dir.path().join("backup.tar.gz");
        let info = s.create_archive(archive.to_str().unwrap()).unwrap();
        assert!(info.size_bytes > 0);
        drop(s); // release WiredTiger's single-writer lock before reopening

        let target = dir.path().join("restored");
        extract_backup_archive(archive.to_str().unwrap(), target.to_str().unwrap()).unwrap();
        assert!(target.join("pitr-manifest.json").exists());

        let restored = Storage::open(target.to_str().unwrap()).unwrap();
        let got = restored.find_matching("app", "c", &doc! {}).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(decode_doc(&got[0]).unwrap().get_i32("v").unwrap(), 7);
    }

    #[test]
    fn resolve_current_date_folds_into_set() {
        // `true` and `{$type: "date"}` → DateTime; `{$type: "timestamp"}` →
        // Timestamp; all merged into an existing $set.
        let upd = doc! {
            "$set": {"status": "P"},
            "$currentDate": {
                "a": true,
                "b": {"$type": "date"},
                "c": {"$type": "timestamp"},
            },
        };
        let out = resolve_current_date(&upd).unwrap();
        assert!(!out.contains_key("$currentDate"));
        let set = out.get_document("$set").unwrap();
        assert_eq!(set.get_str("status").unwrap(), "P");
        assert!(matches!(set.get("a"), Some(Bson::DateTime(_))));
        assert!(matches!(set.get("b"), Some(Bson::DateTime(_))));
        assert!(matches!(set.get("c"), Some(Bson::Timestamp(_))));
        // No $currentDate → returned unchanged.
        let plain = doc! {"$inc": {"n": 1i32}};
        assert_eq!(resolve_current_date(&plain).unwrap(), plain);
        // Unrecognised option → error (mirrors mongod / update.py rejecting it).
        let bad = doc! {"$currentDate": {"x": {"$type": "nope"}}};
        assert!(resolve_current_date(&bad).is_err());
        // A boolean `false` sets the current Date, just like `true` (mongod /
        // update.py accept it as the set-Date form).
        let false_form = doc! {"$currentDate": {"x": false}};
        let out2 = resolve_current_date(&false_form).unwrap();
        assert!(matches!(
            out2.get_document("$set").unwrap().get("x"),
            Some(Bson::DateTime(_))
        ));
    }

    #[test]
    fn query_implies_partial_operator_bounds() {
        // bare equality implies an operator-form partial bound it satisfies.
        assert!(query_implies_partial(
            &doc! {"a": 1i32},
            &doc! {"a": {"$lte": 1.5}}
        ));
        // ... but not one it violates.
        assert!(!query_implies_partial(
            &doc! {"a": 2i32},
            &doc! {"a": {"$lte": 1.5}}
        ));
        // operator-form query implies a looser operator-form partial bound.
        assert!(query_implies_partial(
            &doc! {"a": {"$lte": 1i32}},
            &doc! {"a": {"$lte": 1.5}}
        ));
        assert!(!query_implies_partial(
            &doc! {"a": {"$lte": 1.6}},
            &doc! {"a": {"$lte": 1.5}}
        ));
        // bare equality implies a bare-equality partial of the same value.
        assert!(query_implies_partial(&doc! {"s": "x"}, &doc! {"s": "x"}));
        // missing partial-filter field in the query is never implied.
        assert!(!query_implies_partial(&doc! {"b": 1i32}, &doc! {"a": 1i32}));
    }

    #[test]
    fn insert_stores_client_bytes_verbatim_when_id_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("src");
        std::fs::create_dir_all(&home).unwrap();
        let s = Storage::open(home.to_str().unwrap()).unwrap();

        // `_id` already leading: the raw-write fast path stores the caller's BSON
        // verbatim (no encode_doc round-trip), byte-for-byte.
        let id_first = bson::to_vec(&doc! {"_id": 7i32, "x": 1i32, "s": "hi"}).unwrap();
        s.insert("db", "c", vec![id_first.clone()], true).unwrap();
        let stored = s.find_by_id("db", "c", &Bson::Int32(7)).unwrap().unwrap();
        assert_eq!(stored, id_first, "id-first doc must be stored verbatim");

        // `_id` NOT leading: must be reordered to mongod's canonical `_id`-first
        // storage form (the encode_doc fallback), so the stored bytes differ from
        // the caller's and lead with `_id`.
        let id_last = bson::to_vec(&doc! {"x": 2i32, "_id": 8i32}).unwrap();
        s.insert("db", "c", vec![id_last.clone()], true).unwrap();
        let stored = s.find_by_id("db", "c", &Bson::Int32(8)).unwrap().unwrap();
        assert_ne!(
            stored, id_last,
            "id-not-first must be reordered, not stored raw"
        );
        assert_eq!(
            stored,
            encode_doc(&doc! {"x": 2i32, "_id": 8i32}).unwrap(),
            "reordered form must match encode_doc"
        );
        // First element's key (bytes after the 4-byte length + 1 type byte) is `_id`.
        assert_eq!(&stored[5..8], b"_id", "stored doc must lead with _id");

        // Missing `_id`: the server assigns an ObjectId and stores it leading.
        let no_id = bson::to_vec(&doc! {"only": 1i32}).unwrap();
        s.insert("db", "c", vec![no_id], true).unwrap();
        let all = s.scan_collection("db", "c").unwrap();
        let assigned = all
            .iter()
            .find(|b| decode_doc(b).unwrap().get("only").is_some())
            .expect("assigned-id doc present");
        assert_eq!(
            &assigned[5..8],
            b"_id",
            "assigned-id doc must lead with _id"
        );
        assert!(
            decode_doc(assigned).unwrap().get_object_id("_id").is_ok(),
            "missing _id must be assigned an ObjectId"
        );
    }

    #[test]
    fn partial_index_used_via_residual_when_query_implies_bound() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("src");
        std::fs::create_dir_all(&home).unwrap();
        let s = Storage::open(home.to_str().unwrap()).unwrap();
        s.create_index(
            "app",
            "t",
            "x_1",
            &doc! {"x": 1i32},
            &doc! {"partialFilterExpression": {"a": {"$lte": 1.5}}},
        )
        .unwrap();
        s.insert(
            "app",
            "t",
            vec![
                encode_doc(&doc! {"_id": 1i32, "x": 5i32, "a": 2i32}).unwrap(),
                encode_doc(&doc! {"_id": 2i32, "x": 6i32, "a": 1i32}).unwrap(),
                encode_doc(&doc! {"_id": 3i32, "x": 6i32, "a": 5i32}).unwrap(),
            ],
            true,
        )
        .unwrap();

        // Residual `a:1` is partial-implied → IXSCAN on x_1, and the exact
        // recheck must exclude the unindexed a=5 doc.
        let plan = s
            .explain_plan("app", "t", &doc! {"x": 6i32, "a": 1i32})
            .unwrap();
        assert!(matches!(&plan, ExplainPlan::IxScan { index_name, .. } if index_name == "x_1"));
        let ids: Vec<i32> = s
            .find_matching("app", "t", &doc! {"x": 6i32, "a": 1i32})
            .unwrap()
            .iter()
            .map(|b| decode_doc(b).unwrap().get_i32("_id").unwrap())
            .collect();
        assert_eq!(ids, vec![2]);

        // Operator-form leading clause + partial-implied residual → IXSCAN.
        let plan = s
            .explain_plan("app", "t", &doc! {"x": {"$gt": 1i32}, "a": 1i32})
            .unwrap();
        assert!(matches!(&plan, ExplainPlan::IxScan { index_name, .. } if index_name == "x_1"));

        // Residual `a <= 1.6` does NOT imply `a <= 1.5` → COLLSCAN, but results
        // still correct (only a=1 satisfies a<=1.6 among x=6 docs other than a=5).
        let plan = s
            .explain_plan("app", "t", &doc! {"x": 6i32, "a": {"$lte": 1.6}})
            .unwrap();
        assert!(matches!(plan, ExplainPlan::CollScan));

        // No constraint on the partial field → COLLSCAN, returns both x=6 docs.
        let plan = s.explain_plan("app", "t", &doc! {"x": 6i32}).unwrap();
        assert!(matches!(plan, ExplainPlan::CollScan));
        assert_eq!(
            s.find_matching("app", "t", &doc! {"x": 6i32})
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn capped_collection_evicts_oldest_across_inserts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("src");
        std::fs::create_dir_all(&home).unwrap();
        let s = Storage::open(home.to_str().unwrap()).unwrap();
        s.create_collection_with_options(
            "app",
            "cap",
            &doc! {"capped": true, "size": 1_000_000i64, "max": 3i32},
        )
        .unwrap();
        // One-at-a-time inserts: each is its own batch, so the prior docs are
        // non-fresh and eviction trims to the cap.
        for i in 0..6i32 {
            s.insert(
                "app",
                "cap",
                vec![encode_doc(&doc! {"_id": i}).unwrap()],
                true,
            )
            .unwrap();
        }
        let mut ids: Vec<i32> = s
            .find_matching("app", "cap", &doc! {})
            .unwrap()
            .iter()
            .map(|b| decode_doc(b).unwrap().get_i32("_id").unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![3, 4, 5]);

        // A single over-cap batch keeps all its docs (they're all "fresh").
        s.create_collection_with_options(
            "app",
            "cap2",
            &doc! {"capped": true, "size": 1_000_000i64, "max": 3i32},
        )
        .unwrap();
        s.insert(
            "app",
            "cap2",
            (0..6i32)
                .map(|i| encode_doc(&doc! {"_id": i}).unwrap())
                .collect(),
            true,
        )
        .unwrap();
        assert_eq!(s.find_matching("app", "cap2", &doc! {}).unwrap().len(), 6);
    }

    #[test]
    fn oplog_rs_view_is_findable_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("src");
        std::fs::create_dir_all(&home).unwrap();
        let s = Storage::open(home.to_str().unwrap()).unwrap();
        s.insert(
            "app",
            "c",
            vec![encode_doc(&doc! {"_id": 1i32}).unwrap()],
            true,
        )
        .unwrap();
        // The synthetic view surfaces the persisted oplog entries.
        let entries = s.find_matching("local", "oplog.rs", &doc! {}).unwrap();
        assert!(!entries.is_empty());
        assert!(decode_doc(&entries[0]).unwrap().contains_key("ts"));
        // count routes through the same synthesis.
        assert_eq!(
            s.count_matching("local", "oplog.rs", &doc! {}, None)
                .unwrap(),
            entries.len()
        );
        // Tailable cursors require a capped collection — the view reports capped.
        assert!(s.collection_is_capped("local", "oplog.rs").unwrap());
    }

    #[test]
    fn replay_rebuilds_database_from_oplog() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let empty = doc! {};
        {
            let s = Storage::open(src.to_str().unwrap()).unwrap();
            s.insert(
                "app",
                "c",
                vec![
                    encode_doc(&doc! {"_id": 1i32, "v": 1i32}).unwrap(),
                    encode_doc(&doc! {"_id": 2i32, "v": 2i32}).unwrap(),
                ],
                true,
            )
            .unwrap();
            // operator update -> oplog $v:2 diff; reverse-applied on replay
            s.update_matching(
                "app",
                "c",
                &doc! {"_id": 1i32},
                &doc! {"$set": {"v": 100i32}},
                false,
                false,
                &[],
                &empty,
                None,
                None,
            )
            .unwrap();
            s.delete_matching("app", "c", &doc! {"_id": 2i32}, 1, &empty, None)
                .unwrap();
            s.insert(
                "app",
                "c",
                vec![encode_doc(&doc! {"_id": 3i32, "v": 3i32}).unwrap()],
                true,
            )
            .unwrap();
        } // drop releases the WiredTiger single-writer lock

        let out = dir.path().join("restored");
        let stats = replay::restore_to_timestamp(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            None,
            None,
            false,
        )
        .unwrap();
        assert!(stats.ops_applied >= 4);

        let restored = Storage::open(out.to_str().unwrap()).unwrap();
        let mut got: Vec<(i32, i32)> = restored
            .find_matching("app", "c", &empty)
            .unwrap()
            .iter()
            .map(|b| {
                let d = decode_doc(b).unwrap();
                (d.get_i32("_id").unwrap(), d.get_i32("v").unwrap())
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![(1, 100), (3, 3)]);
    }

    #[test]
    fn replay_reconstructs_collection_options() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let validator = doc! {"v": {"$gt": 0}};
        {
            let s = Storage::open(src.to_str().unwrap()).unwrap();
            let opts = doc! {"capped": true, "size": 8192i64, "max": 100i64, "validator": validator.clone()};
            assert!(s.create_collection_with_options("app", "c", &opts).unwrap());
            s.insert(
                "app",
                "c",
                vec![encode_doc(&doc! {"_id": 1i32, "v": 5i32}).unwrap()],
                true,
            )
            .unwrap();
        }

        let out = dir.path().join("restored");
        replay::restore_to_timestamp(
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            None,
            None,
            false,
        )
        .unwrap();

        let restored = Storage::open(out.to_str().unwrap()).unwrap();
        let got = restored.get_collection_options("app", "c").unwrap();
        assert_eq!(got.get_bool("capped").ok(), Some(true));
        assert_eq!(got.get_i64("size").ok(), Some(8192));
        assert_eq!(got.get_i64("max").ok(), Some(100));
        assert_eq!(got.get_document("validator").ok(), Some(&validator));
    }

    #[test]
    fn restore_with_carry_oplog_preserves_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let src_seqs: Vec<i64> = {
            let s = Storage::open(src.to_str().unwrap()).unwrap();
            s.insert(
                "app",
                "c",
                vec![
                    encode_doc(&doc! {"_id": 1i32}).unwrap(),
                    encode_doc(&doc! {"_id": 2i32}).unwrap(),
                ],
                true,
            )
            .unwrap();
            s.read_oplog(1, 100)
                .unwrap()
                .iter()
                .map(|(q, _)| *q)
                .collect()
        };

        // carry_oplog = true: the restored store keeps the same oplog seqs ...
        let carried = dir.path().join("carried");
        replay::restore_to_timestamp(
            src.to_str().unwrap(),
            carried.to_str().unwrap(),
            None,
            None,
            true,
        )
        .unwrap();
        let r = Storage::open(carried.to_str().unwrap()).unwrap();
        let restored_seqs: Vec<i64> = r
            .read_oplog(1, 100)
            .unwrap()
            .iter()
            .map(|(q, _)| *q)
            .collect();
        assert_eq!(restored_seqs, src_seqs);
        let tail = r.oplog_tail_seq();
        r.insert(
            "app",
            "c",
            vec![encode_doc(&doc! {"_id": 3i32}).unwrap()],
            true,
        )
        .unwrap();
        assert_eq!(r.oplog_tail_seq(), tail + 1); // a fresh write continues the timeline
        drop(r);

        // ... while the default (no carry) leaves the restored oplog empty.
        let fresh = dir.path().join("fresh");
        replay::restore_to_timestamp(
            src.to_str().unwrap(),
            fresh.to_str().unwrap(),
            None,
            None,
            false,
        )
        .unwrap();
        let f = Storage::open(fresh.to_str().unwrap()).unwrap();
        assert!(f.read_oplog(1, 100).unwrap().is_empty());
    }

    #[test]
    fn v2_restore_reaches_before_pruned_floor() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        let archive_s = archive.to_str().unwrap().to_string();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let enc = |id: i32| encode_doc(&doc! {"_id": id}).unwrap();
        {
            let mut s = Storage::open(src.to_str().unwrap()).unwrap();
            s.set_oplog_archive_dir(Some(archive_s.clone()));
            s.set_oplog_max_entries(2);
            s.insert("app", "c", vec![enc(1), enc(2)], true).unwrap();
            s.archive_base_snapshot(&archive_s).unwrap(); // base head = seq 2
            s.insert("app", "c", vec![enc(3)], true).unwrap();
            s.insert("app", "c", vec![enc(4)], true).unwrap();
            s.insert("app", "c", vec![enc(5)], true).unwrap();
            let pruned = s.prune_oplog(None).unwrap(); // cap=2 -> archive seq 1,2,3
            assert!(pruned >= 3, "pruned {pruned}");
            assert_eq!(s.oplog_floor_seq().unwrap(), 4); // live oplog no longer reaches seq 3
        }

        // v1 can't restore the src (floor past genesis); v2 stitches base + segments.
        let out = dir.path().join("restored");
        pitr_archive::restore_from_archive_dir(
            &archive_s,
            out.to_str().unwrap(),
            None,
            None,
            false,
        )
        .unwrap();
        let r = Storage::open(out.to_str().unwrap()).unwrap();
        let mut ids: Vec<i32> = r
            .find_matching("app", "c", &doc! {})
            .unwrap()
            .iter()
            .map(|b| decode_doc(b).unwrap().get_i32("_id").unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]); // base (1,2) + archived seq 3
    }

    #[test]
    fn opportunistic_prune_bounds_oplog_from_writes() {
        // The write path prunes the oplog every OPLOG_PRUNE_INTERVAL emits, so a
        // long-running server bounds its oplog from writes alone — with NO
        // explicit prune_oplog call and NO noop-heartbeat sweeper (the default).
        let dir = tempfile::tempdir().unwrap();
        let mut s = Storage::open(dir.path().to_str().unwrap()).unwrap();
        s.set_oplog_max_entries(10);
        let enc = |id: i32| encode_doc(&doc! {"_id": id}).unwrap();
        let total = OPLOG_PRUNE_INTERVAL as i32 + 1; // cross the threshold once
        for i in 0..total {
            s.insert("app", "c", vec![enc(i)], true).unwrap();
        }
        // The opportunistic prune at the OPLOG_PRUNE_INTERVAL-th emit trimmed the
        // oplog to the 10-entry cap; only the handful of writes after it remain.
        let live = s.read_oplog(1, 1_000_000).unwrap().len();
        assert!(
            live <= 50,
            "oplog not opportunistically pruned: {live} live rows after {total} writes"
        );
        // Pruning the oplog never touches document data.
        assert_eq!(
            s.find_matching("app", "c", &doc! {}).unwrap().len() as i32,
            total
        );
    }

    #[test]
    fn v2_segment_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().to_str().unwrap();
        let rows = vec![
            (
                1i64,
                doc! {"op": "i", "ns": "a.b", "o": {"_id": 1i32}},
                None,
            ),
            (
                2i64,
                doc! {"op": "i", "ns": "a.b", "o": {"_id": 2i32}},
                Some(doc! {"_id": 2i32, "old": true}),
            ),
        ];
        pitr_archive::write_segment(archive, &rows).unwrap();
        let got = pitr_archive::iter_archived_oplog(archive).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 1);
        assert_eq!(got[1].2, Some(doc! {"_id": 2i32, "old": true}));
        assert!(pitr_archive::is_archive_dir(archive));
    }

    #[test]
    fn escape_kb_doubles_zero_bytes() {
        assert_eq!(escape_kb(b"\x01\x00\x02"), vec![0x01, 0x00, 0xff, 0x02]);
        assert_eq!(escape_kb(b"abc"), b"abc".to_vec());
        assert_eq!(escape_kb(b"\x00\x00"), vec![0x00, 0xff, 0x00, 0xff]);
    }

    #[test]
    fn pack_entry_layout_and_unpack_roundtrip() {
        // `escape(kb) + \x00\x00 + RecordId(8B big-endian)` (step 2).
        let kb = b"\x01\x00\x02".as_slice();
        let packed = pack_entry(kb, 1);
        assert_eq!(
            packed,
            vec![0x01, 0x00, 0xff, 0x02, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        let (esc_kb, rid) = unpack_entry(&packed);
        assert_eq!(esc_kb, escape_kb(kb).as_slice());
        assert_eq!(rid, Some(1));
    }

    #[test]
    fn unpack_splits_on_first_separator() {
        // A RecordId's big-endian bytes routinely contain `\x00\x00` (every small
        // id does), so the split MUST land at the FIRST separator — correct
        // because the escaped kb half can never contain a bare `\x00\x00`, and
        // safe because the trailing half is fixed-width and taken whole.
        let kb = b"\x00".as_slice();
        let packed = pack_entry(kb, 1);
        let (esc_kb, rid) = unpack_entry(&packed);
        assert_eq!(esc_kb, vec![0x00, 0xff].as_slice());
        assert_eq!(rid, Some(1));
    }

    #[test]
    fn pack_entry_orders_by_recordid_within_a_key() {
        // Big-endian keeps the B-tree ordered by RecordId (insertion order)
        // within one index key — little-endian would scramble it.
        let kb = b"k".as_slice();
        assert!(pack_entry(kb, 1) < pack_entry(kb, 2));
        assert!(pack_entry(kb, 2) < pack_entry(kb, 300));
        assert!(pack_entry(kb, 300) < pack_entry(kb, i64::MAX));
    }

    #[test]
    fn unpack_rejects_a_step1_entry() {
        // A step-1 entry's trailing half is an `id_key`, not an 8-byte RecordId.
        // It must report None rather than reinterpreting those bytes as a
        // RecordId — that would silently fetch the wrong document.
        let mut packed = escape_kb(b"k");
        packed.extend_from_slice(ENTRY_SEP);
        packed.extend_from_slice(b"an-id-key");
        let (_esc, rid) = unpack_entry(&packed);
        assert_eq!(rid, None);
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

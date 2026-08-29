//! Oplog replay for point-in-time recovery — the Rust port of
//! `secantus.oplog_replay` (`_apply_entry` / `replay` / `restore_to_timestamp`).
//!
//! Replays a stopped source database's oplog forward into a fresh target,
//! stopping at a target timestamp. Each entry is applied through the ordinary
//! `Storage` write paths (insert / update / delete / DDL) with oplog emission
//! suppressed (`set_enable_oplog(false)` on the owned target), so the documents,
//! indexes, and natural order are rebuilt exactly as they were produced live.
//! Updates carrying a `$v: 2` diff are rolled forward with
//! `secantus_core::diff::apply_update_description`.
//!
//! The on-disk + oplog formats are identical to the Python server, so this
//! restores a Python-written backup and vice versa.

use bson::{Bson, Document, Timestamp};

use crate::{Storage, StorageError};

/// Genesis is seq 1: an empty-base replay is exact only when the source oplog
/// still reaches it (hasn't been pruned from the front). Mirrors Python `_GENESIS_SEQ`.
const GENESIS_SEQ: i64 = 1;
const READ_CHUNK: usize = 2000;

type Result<T> = std::result::Result<T, StorageError>;

/// Outcome of a replay: how much was applied and the position reached.
#[derive(Debug, Default)]
pub struct ReplayStats {
    pub ops_applied: u64,
    pub entries_seen: u64,
    pub last_seq: i64,
}

fn ns_split(ns: &str) -> (String, String) {
    match ns.split_once('.') {
        Some((db, coll)) => (db.to_string(), coll.to_string()),
        None => (ns.to_string(), String::new()),
    }
}

fn empty_doc() -> Document {
    Document::new()
}

/// Apply a `c` (command) oplog entry's `o` to the matching Storage DDL. Mirrors
/// the `c` shapes Storage emits (create / drop / dropDatabase / createIndexes /
/// dropIndexes / collMod / renameCollection).
fn apply_command(storage: &Storage, db: &str, o: &Document) -> Result<()> {
    if let Ok(coll) = o.get_str("create") {
        // Collection options (capped / size / max / validator / viewOn / …) ride
        // the create entry as siblings of `create`; reconstruct them.
        let options: Document = o
            .iter()
            .filter(|(k, _)| k.as_str() != "create" && k.as_str() != "idIndex")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        storage.create_collection_with_options(db, coll, &options)?;
    } else if let Ok(coll) = o.get_str("drop") {
        storage.drop_collection(db, coll)?;
    } else if o.contains_key("dropDatabase") {
        storage.drop_database(db)?;
    } else if let Ok(coll) = o.get_str("createIndexes") {
        if let Ok(indexes) = o.get_array("indexes") {
            for spec in indexes {
                let Bson::Document(spec) = spec else { continue };
                let (Ok(name), Ok(key)) = (spec.get_str("name"), spec.get_document("key")) else {
                    continue;
                };
                let options: Document = spec
                    .iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "v" | "key" | "name"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                storage.create_index(db, coll, name, key, &options)?;
            }
        }
    } else if let Ok(coll) = o.get_str("dropIndexes") {
        if let Ok(name) = o.get_str("index") {
            storage.drop_index(db, coll, name)?;
        }
    } else if let Ok(coll) = o.get_str("collMod") {
        // A `collMod {index: {name, expireAfterSeconds}}` retunes a TTL index;
        // everything else (validator / changeStreamPreAndPostImages / …) is a
        // plain option write.
        if let Ok(index) = o.get_document("index") {
            if let (Ok(name), Some(expiry)) = (
                index.get_str("name"),
                index
                    .get_i64("expireAfterSeconds")
                    .ok()
                    .or_else(|| index.get_i32("expireAfterSeconds").ok().map(i64::from)),
            ) {
                storage.set_index_expiry(db, coll, name, expiry)?;
            }
        }
        let desc: Document = o
            .iter()
            .filter(|(k, _)| k.as_str() != "collMod" && k.as_str() != "index")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !desc.is_empty() {
            storage.coll_mod(db, coll, &desc)?;
        }
    } else if let Ok(rename_from) = o.get_str("renameCollection") {
        if let Ok(to) = o.get_str("to") {
            let (sdb, scoll) = ns_split(rename_from);
            let (ddb, dcoll) = ns_split(to);
            storage.rename_collection(&sdb, &scoll, &ddb, &dcoll, o.contains_key("dropTarget"))?;
        }
    }
    // Unknown commands (e.g. an internal noop wrapped as 'c') are ignored.
    Ok(())
}

/// Apply one oplog entry to `storage`. Returns `Ok(true)` if it mutated state.
/// `storage` must have oplog emission disabled so the write paths don't re-emit.
pub fn apply_entry(storage: &Storage, entry: &Document) -> Result<bool> {
    let op = entry.get_str("op").unwrap_or("");
    if op == "n" {
        return Ok(false); // periodic noop — changes nothing
    }
    let ns = entry.get_str("ns").unwrap_or("");
    if op == "c" {
        let (db, _) = ns_split(ns);
        if let Ok(o) = entry.get_document("o") {
            apply_command(storage, &db, o)?;
        }
        return Ok(true);
    }
    let (db, coll) = ns_split(ns);
    match op {
        "i" => {
            let o = entry
                .get_document("o")
                .map_err(|_| StorageError::Internal("replay: insert entry missing o".into()))?;
            let mut buf = Vec::new();
            o.to_writer(&mut buf)
                .map_err(|e| StorageError::Bson(e.to_string()))?;
            storage.insert(&db, &coll, vec![buf], true)?;
            Ok(true)
        }
        "u" => {
            let id = match entry.get_document("o2").ok().and_then(|o2| o2.get("_id")) {
                Some(id) => id.clone(),
                None => return Ok(false),
            };
            let filter = bson::doc! { "_id": id };
            let o = entry
                .get_document("o")
                .map_err(|_| StorageError::Internal("replay: update entry missing o".into()))?;
            let post: Document = if o.get_i32("$v").ok() == Some(2) && o.contains_key("diff") {
                let existing = storage.find_matching(&db, &coll, &filter)?;
                let Some(blob) = existing.into_iter().next() else {
                    // In-order replay should never reach an update for a missing
                    // doc; tolerate rather than corrupt the restore.
                    return Ok(false);
                };
                let pre = Document::from_reader(&mut std::io::Cursor::new(blob))
                    .map_err(|e| StorageError::Bson(e.to_string()))?;
                let diff = o.get_document("diff").unwrap();
                secantus_core::diff::apply_update_description(pre, diff)
                    .map_err(|_| StorageError::Internal("replay: diff apply fell back".into()))?
            } else {
                o.clone() // full-document replacement
            };
            storage.update_matching(
                &db,
                &coll,
                &filter,
                &post,
                false,
                false,
                &[],
                &empty_doc(),
                None,
                // Replay re-applies a write that already passed validation when
                // it was first accepted, so no validator is enforced here.
                None,
                false,
            )?;
            Ok(true)
        }
        "d" => {
            let id = entry
                .get_document("o2")
                .ok()
                .and_then(|o2| o2.get("_id").cloned())
                .or_else(|| {
                    entry
                        .get_document("o")
                        .ok()
                        .and_then(|o| o.get("_id").cloned())
                });
            let Some(id) = id else { return Ok(false) };
            storage.delete_matching(
                &db,
                &coll,
                &bson::doc! { "_id": id },
                1,
                &empty_doc(),
                None,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Is `entry` at or before the target? An entry past the bound stops replay.
/// Both `ts` and `wall` are shared across a transaction's statements, so the cut
/// never splits a transaction.
fn within_bound(entry: &Document, up_to_ts: Option<Timestamp>, up_to_wall_ms: Option<i64>) -> bool {
    if let Some(bound) = up_to_ts {
        if let Some(Bson::Timestamp(ts)) = entry.get("ts") {
            if (ts.time, ts.increment) > (bound.time, bound.increment) {
                return false;
            }
        }
    }
    if let Some(bound_ms) = up_to_wall_ms {
        if let Some(Bson::DateTime(dt)) = entry.get("wall") {
            if dt.timestamp_millis() > bound_ms {
                return false;
            }
        }
    }
    true
}

/// Replay `source`'s oplog into `target` (which must have oplog emission
/// disabled), stopping before the first entry past the target time. When `carry`
/// is set, the in-bound rows are also collected (verbatim) for the caller to
/// re-import onto the target so the restored oplog timeline is preserved.
fn replay(
    source: &Storage,
    target: &Storage,
    up_to_ts: Option<Timestamp>,
    up_to_wall_ms: Option<i64>,
    carry: bool,
) -> Result<(ReplayStats, Vec<(i64, Document)>)> {
    let mut stats = ReplayStats::default();
    let mut carried: Vec<(i64, Document)> = Vec::new();
    let mut start = source.oplog_floor_seq()?;
    if start == 0 {
        return Ok((stats, carried));
    }
    'outer: loop {
        let batch = source.read_oplog(start, READ_CHUNK)?;
        if batch.is_empty() {
            break;
        }
        for (seq, blob) in &batch {
            let entry = Document::from_reader(&mut std::io::Cursor::new(blob.clone()))
                .map_err(|e| StorageError::Bson(e.to_string()))?;
            if !within_bound(&entry, up_to_ts, up_to_wall_ms) {
                break 'outer;
            }
            stats.entries_seen += 1;
            stats.last_seq = *seq;
            if carry {
                carried.push((*seq, entry.clone()));
            }
            if apply_entry(target, &entry)? {
                stats.ops_applied += 1;
            }
        }
        start = batch[batch.len() - 1].0 + 1;
    }
    Ok((stats, carried))
}

/// Replay a pre-built `(seq, entry)` list (already in ascending seq order) into
/// `target`, stopping before the first entry past the bound. Used by PITR v2
/// restore, whose rows are stitched from mixed sources (archived segments + base
/// snapshots) rather than streamed from one source oplog.
pub fn replay_entries(
    target: &Storage,
    rows: &[(i64, Document)],
    up_to_ts: Option<Timestamp>,
    up_to_wall_ms: Option<i64>,
    carry: bool,
) -> Result<(ReplayStats, Vec<(i64, Document)>)> {
    let mut stats = ReplayStats::default();
    let mut carried: Vec<(i64, Document)> = Vec::new();
    for (seq, entry) in rows {
        if !within_bound(entry, up_to_ts, up_to_wall_ms) {
            break;
        }
        stats.entries_seen += 1;
        stats.last_seq = *seq;
        if carry {
            carried.push((*seq, entry.clone()));
        }
        if apply_entry(target, entry)? {
            stats.ops_applied += 1;
        }
    }
    Ok((stats, carried))
}

/// Rebuild `target_dir` as the database was at the target time by replaying
/// `source_dir`'s oplog forward. `source_dir` is a stopped server's data
/// directory (or an extracted backup archive); `target_dir` must be fresh. With
/// no bound, the whole oplog is replayed ("latest"). Mirrors Python
/// `oplog_replay.restore_to_timestamp`.
pub fn restore_to_timestamp(
    source_dir: &str,
    target_dir: &str,
    up_to_ts: Option<Timestamp>,
    up_to_wall_ms: Option<i64>,
    carry_oplog: bool,
) -> Result<ReplayStats> {
    let source = Storage::open(source_dir)?;
    let floor = source.oplog_floor_seq()?;
    if floor == 0 {
        return Err(StorageError::Internal(
            "source has no oplog to replay (was it created with enable_oplog?)".into(),
        ));
    }
    if floor > GENESIS_SEQ {
        return Err(StorageError::Internal(format!(
            "source oplog floor is seq {floor}, past genesis (seq {GENESIS_SEQ}): it has been \
             pruned from the front, so an empty-base replay would be partial"
        )));
    }
    std::fs::create_dir_all(target_dir)
        .map_err(|e| StorageError::Internal(format!("restore: create target dir: {e}")))?;
    let mut target = Storage::open(target_dir)?;
    target.set_enable_oplog(false); // replay drives the write paths without re-emitting
    let (stats, carried) = replay(&source, &target, up_to_ts, up_to_wall_ms, carry_oplog)?;
    if carry_oplog && !carried.is_empty() {
        // Preserve the replayed oplog timeline: write the in-bound rows verbatim
        // (with pre-images) so a change stream on the restored server resumes
        // from a token minted before the restore point. Default (no carry) leaves
        // a fresh empty timeline, like mongorestore.
        let mut pre_images: std::collections::HashMap<i64, Vec<u8>> =
            std::collections::HashMap::new();
        for (seq, _) in &carried {
            if let Some(pre) = source.read_preimage(*seq)? {
                pre_images.insert(*seq, pre);
            }
        }
        target.import_oplog_segment(&carried, &pre_images)?;
    }
    target.checkpoint()?;
    Ok(stats)
}

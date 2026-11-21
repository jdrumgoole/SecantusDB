//! PITR v2 — arbitrary-window recovery via archived oplog segments + base
//! snapshots. The Rust port of `secantus.pitr_archive`.
//!
//! v1 (`crate::replay`) replays an oplog onto an empty base, so its window is the
//! live oplog retention window. v2 keeps two durable artifacts in an **archive
//! directory**:
//!
//! * **Oplog segments** (`oplog-<start>-<end>.seg`) — the rows `prune_oplog` is
//!   about to drop, written out first (see `Storage::prune_oplog` when an
//!   `oplog_archive_dir` is configured). Each is a stream of length-framed BSON
//!   docs `{s: seq, e: entry, p: pre_image?}`.
//! * **Base snapshots** (`base-<head>.tar.gz`) — ordinary backup archives
//!   (`Storage::archive_base_snapshot`), named by their oplog head seq.
//!
//! A restore to time `T` picks the newest base whose head is ≤ `T`, extracts it,
//! and replays the oplog forward from the base's head — stitching the rows from
//! the archived segments and the newest snapshot's still-live oplog (deduped by
//! seq). Same on-disk layout as the Python server, so either reads the other's
//! archive.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use bson::{Bson, Document, Timestamp};

use crate::replay::ReplayStats;
use crate::{extract_backup_archive, Storage, StorageError};

type Result<T> = std::result::Result<T, StorageError>;

const SEG_PREFIX: &str = "oplog-";
const SEG_SUFFIX: &str = ".seg";
const BASE_PREFIX: &str = "base-";
const BASE_SUFFIX: &str = ".tar.gz";

fn io_err(ctx: &str, e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(format!("{ctx}: {e}"))
}

/// Filename for a segment covering `[start, end]` (positions are oplog
/// timestamps; zero-padded so a lexical listing is also in order). `prev` is
/// the position of the entry just before the segment's first -- 0 when the
/// segment starts the oplog -- so segments chain by abutting: a segment
/// follows another when its `prev` is the other's `end`.
pub fn segment_name(start: i64, end: i64, prev: i64) -> String {
    format!("{SEG_PREFIX}{start:020}-{end:020}-{prev:020}{SEG_SUFFIX}")
}

/// `(start, end, prev)` from a segment's file name.
fn parse_segment_name(name: &str) -> Option<(i64, i64, i64)> {
    let inner = name.strip_prefix(SEG_PREFIX)?.strip_suffix(SEG_SUFFIX)?;
    let mut it = inner.split('-');
    let start = it.next()?.parse().ok()?;
    let end = it.next()?.parse().ok()?;
    let prev = it.next()?.parse().ok()?;
    it.next().is_none().then_some((start, end, prev))
}

/// Filename for a base snapshot whose oplog head is `head`.
pub fn base_name(head: i64) -> String {
    format!("{BASE_PREFIX}{head:020}{BASE_SUFFIX}")
}

/// Append `rows` (`(position, entry, pre_image)` in order; `prev` as for
/// [`segment_name`]) as one segment file in
/// `archive_dir`. Returns the path, or `None` if no rows. Writes to a temp file
/// then atomically renames so a crash never leaves a half-segment.
pub fn write_segment(
    archive_dir: &str,
    rows: &[(i64, Document, Option<Document>)],
    prev: i64,
) -> Result<Option<String>> {
    let (Some((first, _, _)), Some((last, _, _))) = (rows.first(), rows.last()) else {
        return Ok(None);
    };
    let mut framed: Vec<u8> = Vec::new();
    for (seq, entry, pre) in rows {
        let mut doc = Document::new();
        doc.insert("s", *seq);
        doc.insert("e", Bson::Document(entry.clone()));
        if let Some(p) = pre {
            doc.insert("p", Bson::Document(p.clone()));
        }
        let mut blob = Vec::new();
        secantus_core::bsondoc::write(&doc, &mut blob)
            .map_err(|e| StorageError::Bson(e.to_string()))?;
        framed.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        framed.extend_from_slice(&blob);
    }
    std::fs::create_dir_all(archive_dir).map_err(|e| io_err("write_segment", e))?;
    let path = Path::new(archive_dir).join(segment_name(*first, *last, prev));
    let tmp = Path::new(archive_dir).join(format!(".{}.part", segment_name(*first, *last, prev)));
    // The rows this segment holds are deleted from WiredTiger once it is
    // written, and the next checkpoint makes that delete durable: the
    // segment must be on disk first, its name included, or a power loss
    // leaves a permanent gap in the recovery chain (RM-190).
    std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(&framed)?;
            f.sync_all()
        })
        .map_err(|e| io_err("write_segment", e))?;
    std::fs::rename(&tmp, &path).map_err(|e| io_err("write_segment", e))?;
    sync_dir(Path::new(archive_dir)).map_err(|e| io_err("write_segment", e))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Sync a directory, so a file created or renamed in it keeps its name
/// through a power loss. A Unix notion: Windows will not open a directory
/// as a file ("Access is denied"), and NTFS journals the rename itself.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

fn read_segment_file(path: &Path) -> Result<Vec<(i64, Document, Option<Document>)>> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut data))
        .map_err(|e| io_err("read_segment", e))?;
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 4 <= data.len() {
        let len =
            u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        off += 4;
        if off + len > data.len() {
            break;
        }
        let doc = secantus_core::bsondoc::from_bytes(&data[off..off + len])
            .map_err(|e| StorageError::Bson(e.to_string()))?;
        off += len;
        let seq = doc
            .get_i64("s")
            .or_else(|_| doc.get_i32("s").map(i64::from))
            .unwrap_or(0);
        let entry = doc.get_document("e").cloned().unwrap_or_default();
        let pre = doc.get_document("p").ok().cloned();
        out.push((seq, entry, pre));
    }
    Ok(out)
}

fn glob_suffixed(archive_dir: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(archive_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) && name.ends_with(suffix) {
                out.push(e.path().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// Yield `(seq, entry, pre_image)` from every segment in `archive_dir`, in seq
/// order (segments are non-overlapping, named by their range).
pub fn iter_archived_oplog(archive_dir: &str) -> Result<Vec<(i64, Document, Option<Document>)>> {
    let mut out = Vec::new();
    for path in glob_suffixed(archive_dir, SEG_PREFIX, SEG_SUFFIX) {
        out.extend(read_segment_file(Path::new(&path))?);
    }
    Ok(out)
}

/// Each archived segment's first position and the position its first entry
/// follows (`prev`), read from the file names: `first -> prev`.
pub fn segment_claims(archive_dir: &str) -> BTreeMap<i64, i64> {
    let mut out = BTreeMap::new();
    for path in glob_suffixed(archive_dir, SEG_PREFIX, SEG_SUFFIX) {
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some((start, _end, prev)) = parse_segment_name(&name) {
            out.insert(start, prev);
        }
    }
    out
}

/// The last position the archive's segments reach, if it holds any.
pub fn newest_segment_end(archive_dir: &str) -> Option<i64> {
    glob_suffixed(archive_dir, SEG_PREFIX, SEG_SUFFIX)
        .iter()
        .filter_map(|path| {
            let name = Path::new(path).file_name()?.to_string_lossy().into_owned();
            parse_segment_name(&name).map(|(_, end, _)| end)
        })
        .max()
}

/// `[(head_seq, path), ...]` for the base snapshots in `archive_dir`, ascending.
pub fn list_base_snapshots(archive_dir: &str) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for path in glob_suffixed(archive_dir, BASE_PREFIX, BASE_SUFFIX) {
        let stem = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let inner = &stem[BASE_PREFIX.len()..stem.len() - BASE_SUFFIX.len()];
        if let Ok(head) = inner.parse::<i64>() {
            out.push((head, path));
        }
    }
    out.sort();
    out
}

/// True if `path` is a PITR v2 archive directory (holds ≥1 base snapshot or oplog
/// segment) — distinguishes it from a stopped server's WiredTiger data dir.
pub fn is_archive_dir(path: &str) -> bool {
    if !Path::new(path).is_dir() {
        return false;
    }
    !list_base_snapshots(path).is_empty() || !glob_suffixed(path, SEG_PREFIX, SEG_SUFFIX).is_empty()
}

/// Read the embedded `pitr-manifest.json` from a base snapshot tar without
/// extracting the WiredTiger files. `None` if absent/unreadable.
pub fn read_base_manifest(base_path: &str) -> Option<serde_json::Value> {
    let file = std::fs::File::open(base_path).ok()?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let is_manifest = entry
            .path()
            .ok()
            .map(|p| p.to_string_lossy() == crate::PITR_MANIFEST_NAME)
            .unwrap_or(false);
        if is_manifest {
            let mut buf = String::new();
            entry.read_to_string(&mut buf).ok()?;
            return serde_json::from_str(&buf).ok();
        }
    }
    None
}

fn manifest_head_ts(manifest: &serde_json::Value) -> Option<Timestamp> {
    let arr = manifest.get("oplogHeadTs")?.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    Some(Timestamp {
        time: arr[0].as_i64()? as u32,
        increment: arr[1].as_i64()? as u32,
    })
}

fn manifest_head_wall_ms(manifest: &serde_json::Value) -> Option<i64> {
    let s = manifest.get("oplogHeadWall")?.as_str()?;
    bson::DateTime::parse_rfc3339_str(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

fn head_within_bound(
    manifest: &serde_json::Value,
    to_ts: Option<Timestamp>,
    to_wall_ms: Option<i64>,
) -> bool {
    if let Some(bound) = to_ts {
        return match manifest_head_ts(manifest) {
            Some(h) => (h.time, h.increment) <= (bound.time, bound.increment),
            None => false,
        };
    }
    if let Some(bound) = to_wall_ms {
        return matches!(manifest_head_wall_ms(manifest), Some(h) if h <= bound);
    }
    true // no bound → "latest", every base qualifies
}

/// Choose the newest base snapshot whose oplog head is at or before the target,
/// or `None` to replay onto an empty base (segments must reach genesis).
pub fn select_base(
    archive_dir: &str,
    to_ts: Option<Timestamp>,
    to_wall_ms: Option<i64>,
) -> Option<(i64, String)> {
    let mut chosen = None;
    for (head, path) in list_base_snapshots(archive_dir) {
        let manifest = read_base_manifest(&path).unwrap_or(serde_json::Value::Null);
        if head_within_bound(&manifest, to_ts, to_wall_ms) {
            chosen = Some((head, path));
        }
    }
    chosen
}

/// PITR v2: rebuild `target_dir` as of the target time from an archive directory
/// of base snapshots + oplog segments. Mirrors Python
/// `pitr_archive.restore_from_archive_dir`.
pub fn restore_from_archive_dir(
    archive_dir: &str,
    target_dir: &str,
    to_ts: Option<Timestamp>,
    to_wall_ms: Option<i64>,
    carry_oplog: bool,
) -> Result<ReplayStats> {
    let base = select_base(archive_dir, to_ts, to_wall_ms);
    let base_head = base.as_ref().map(|(h, _)| *h).unwrap_or(0);

    std::fs::create_dir_all(target_dir).map_err(|e| io_err("restore_from_archive_dir", e))?;
    if let Some((_, path)) = &base {
        extract_backup_archive(path, target_dir)?;
    }

    // Gather post-base oplog rows (position > base_head): archived segments
    // plus the newest snapshot's still-live oplog. Deduped by position (an
    // entry's timestamp is immutable). `claims` says, for the first row of
    // each segment and of the live oplog, which entry it directly follows.
    let mut rows: BTreeMap<i64, Document> = BTreeMap::new();
    let mut pre_map: std::collections::HashMap<(i64, i64), Vec<u8>> =
        std::collections::HashMap::new();
    let mut claims = segment_claims(archive_dir);
    for (seq, entry, pre) in iter_archived_oplog(archive_dir)? {
        if seq > base_head {
            rows.entry(seq).or_insert(entry);
            // `p`: each write's pre-image by its place in the entry.
            if let Some(p) = pre {
                for (index, v) in &p {
                    let (Ok(index), Bson::Document(v)) = (index.parse::<i64>(), v) else {
                        continue;
                    };
                    let mut buf = Vec::new();
                    secantus_core::bsondoc::write(v, &mut buf)
                        .map_err(|e| StorageError::Bson(e.to_string()))?;
                    pre_map.entry((seq, index)).or_insert(buf);
                }
            }
        }
    }

    let bases = list_base_snapshots(archive_dir);
    if let Some((newest_head, newest_path)) = bases.last() {
        if *newest_head > base_head {
            let tmp = tempfile::tempdir().map_err(|e| io_err("restore_from_archive_dir", e))?;
            let tmp_path = tmp.path().to_string_lossy().into_owned();
            extract_backup_archive(newest_path, &tmp_path)?;
            let src = Storage::open(&tmp_path)?;
            let floor = src.oplog_floor_seq()?;
            if floor > 0 {
                // What precedes the floor: nothing when the snapshot's oplog
                // still begins at the initiating set; otherwise what was
                // truncated is not known, claimed as just before the floor.
                claims.insert(
                    floor,
                    if src.oplog_reaches_genesis()? {
                        0
                    } else {
                        floor - 1
                    },
                );
            }
            let mut start = base_head + 1;
            loop {
                let batch = src.read_oplog(start, 2000)?;
                if batch.is_empty() {
                    break;
                }
                for (seq, blob) in &batch {
                    if *seq > base_head {
                        let entry = secantus_core::bsondoc::from_bytes(blob)
                            .map_err(|e| StorageError::Bson(e.to_string()))?;
                        rows.entry(*seq).or_insert(entry);
                        if carry_oplog {
                            for (index, pre) in src.read_preimages_of(*seq)? {
                                pre_map.entry((*seq, index)).or_insert(pre);
                            }
                        }
                    }
                }
                start = batch[batch.len() - 1].0 + 1;
            }
        }
    }

    // The run of rows that chains from the base's head: a row follows the
    // last one taken when it is the next of its segment (no claim of its own)
    // or its segment's `prev` names the last one taken. The first row only
    // needs its `prev` at or before the base head (nothing lies between).
    // Replay can't skip a missing entry.
    let mut contiguous: Vec<(i64, Document)> = Vec::new();
    let mut last_taken = base_head;
    let mut gap_after = base_head;
    for (seq, entry) in &rows {
        if let Some(prev) = claims.get(seq) {
            let chains = if contiguous.is_empty() {
                *prev <= base_head
            } else {
                *prev == last_taken
            };
            if !chains {
                break;
            }
        } else if contiguous.is_empty() && base_head == 0 {
            // From genesis the first row must start the oplog: it has to
            // carry a claim of `prev == 0`.
            break;
        }
        contiguous.push((*seq, entry.clone()));
        last_taken = *seq;
        gap_after = *seq;
    }
    let has_more_past_gap = rows.keys().last().map(|m| *m > gap_after).unwrap_or(false);

    let mut target = Storage::open(target_dir)?;
    target.set_enable_oplog(false);
    let (stats, carried) =
        crate::replay::replay_entries(&target, &contiguous, to_ts, to_wall_ms, carry_oplog)?;
    if carry_oplog && !carried.is_empty() {
        let carried_pre: std::collections::HashMap<(i64, i64), Vec<u8>> = pre_map
            .iter()
            .filter(|((seq, _), _)| carried.iter().any(|(c, _)| c == seq))
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        target.import_oplog_segment(&carried, &carried_pre)?;
    }
    target.checkpoint()?;

    // A bound set but never reached, with rows existing past a gap → the archive
    // is incomplete: fail loudly rather than return a truncated database.
    let bound_set = to_ts.is_some() || to_wall_ms.is_some();
    let reached = (stats.entries_seen as usize) < contiguous.len() || !bound_set;
    if bound_set && !reached && has_more_past_gap {
        return Err(StorageError::Internal(format!(
            "archived oplog has a gap after position {gap_after}: cannot reach the requested recovery \
             time — take more frequent base snapshots"
        )));
    }
    Ok(stats)
}

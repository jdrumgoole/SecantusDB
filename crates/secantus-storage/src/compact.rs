//! Giving disk space back: `compact`, as mongod runs it, over this store's
//! shared tables.
//!
//! mongod's `compact` runs WiredTiger's compaction (`WT_SESSION::compact`)
//! over the collection's own table and then each of its indexes', and
//! answers `bytesFreed`: the collection's storage size plus its indexes'
//! before, less the same after, and never below zero (a collection written
//! to while it compacts can end larger). Compaction moves the blocks at the
//! end of a file into the free space nearer its start and truncates the
//! file; a file is left alone unless at least `freeSpaceTargetMB` (default
//! 20 MB) of it is free, and WiredTiger then needs 1 MB and a tenth or a
//! fifth of the file free in its first 90% or 80%.
//!
//! Here a collection has no table of its own: its documents are rows in one
//! of the sixteen document shards ([`super::doc_table_for`]), its `_id`
//! index rows in the shared `_id` table, its other indexes' in the shared
//! entries table (and its unique keys in the claims table). Those are the
//! files `compact` compacts, so what it gives back is the free space in
//! files other collections share -- a dropped or emptied neighbour's pages
//! included -- and `bytesFreed` is what those files shrank by. That is
//! mongod's measure (file sizes before and after) over the files this store
//! keeps the collection in.
//!
//! Two parts of mongod's command are WiredTiger 11.3 features the vendored
//! WiredTiger (11.2, mongodb-7.0) does not have, and are modelled on the
//! WiredTiger 11.3 source (`src/block/block_compact.c`):
//!
//! * `free_space_target`: 11.3 skips a file whose free bytes (the block
//!   manager's available list, the statistic `block-manager: file bytes
//!   available for reuse`) are below the target. That check is made here,
//!   from the same statistic, before WiredTiger is asked; WiredTiger's own
//!   1 MB / 10% / 20% rule, unchanged between the two versions, follows.
//! * `dryRun`: 11.3 runs only its estimation phase and mongod reports the
//!   statistic it leaves (`btree compact bytes rewritten expected`). 11.2 has
//!   no estimation phase. mongod 8.2.11 answered `estimatedBytesFreed: 0` on
//!   every shape measured (2026-10-26: five collections of 6,000 to 200,000
//!   documents, files of 13 to 155 MB with 12 to 103 MB free, the default
//!   target and 1 MB), and a dry run here compacts nothing and answers 0
//!   (`tasks/backlog-rust-mongodb.md`, the dry-run item).

use std::path::Path;
use std::sync::MutexGuard;

use super::*;
pub use crate::auto_compact::AutoCompactOutcome;
use crate::auto_compact::AutoCompactor;

/// What a `compact` covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactTarget<'a> {
    /// A collection: the tables its documents and indexes are kept in.
    Collection(&'a str, &'a str),
    /// The oplog (`local.oplog.rs`): its shard tables.
    Oplog,
    /// The catalog, where a database's view definitions are kept
    /// (`system.views`).
    Catalog,
}

/// How a `compact` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactOutcome {
    /// It ran: the bytes the files gave back (or, for a dry run, the
    /// estimate), never below zero.
    Freed(i64),
    /// Another `compact` of the same target is running (mongod: 96
    /// OperationFailed, "Compaction is already in progress for <ns>").
    AlreadyRunning,
    /// WiredTiger answered `EBUSY` for this table: eviction could not keep
    /// up, or another session held the table exclusively (mongod: 11601
    /// Interrupted, "Compaction interrupted on <table>").
    Busy(String),
}

/// The `background-compact` statistics `serverStatus.wiredTiger` reports,
/// in WiredTiger 11.3's order.
pub const BACKGROUND_STATISTICS: [&str; 12] = crate::auto_compact::STATISTICS;

/// mongod's default `freeSpaceTargetMB` (WiredTiger 11.3's
/// `free_space_target=20MB`).
pub const DEFAULT_FREE_SPACE_TARGET_MB: i64 = 20;

/// The description of the block manager's free-bytes statistic, as a
/// table's `statistics:` cursor names it.
const AVAILABLE_FOR_REUSE: &str = "block-manager: file bytes available for reuse";

/// `MB` megabytes in bytes, as WiredTiger reads `free_space_target=<n>MB`
/// (saturating: a target past the largest file skips every file).
pub(crate) fn megabytes(mb: i64) -> u64 {
    (mb.max(0) as u64).saturating_mul(1 << 20)
}

/// The file WiredTiger keeps `table:<name>` in (one column group, the
/// default): `<home>/<name>.wt`.
pub(crate) fn table_file(home: &str, table: &str) -> std::path::PathBuf {
    Path::new(home).join(format!("{}.wt", table.trim_start_matches("table:")))
}

/// The size of a table's file, 0 when it has none (a shard nothing has
/// written to yet).
pub(crate) fn file_size(home: &str, table: &str) -> Result<u64> {
    match std::fs::metadata(table_file(home, table)) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(StorageError::Internal(format!(
            "could not read the size of {table}'s file: {e}"
        ))),
    }
}

/// The bytes free for reuse in `table`'s file, from its statistics.
pub(crate) fn available_bytes(session: &Session, table: &str) -> Result<u64> {
    let cursor = session.open_cursor(&format!("statistics:{table}"), None)?;
    while cursor.next()? {
        let (description, value) = cursor.get_value_stat()?;
        if description == AVAILABLE_FOR_REUSE {
            return Ok(value.max(0) as u64);
        }
    }
    Err(StorageError::Internal(format!(
        "{table}'s statistics have no \"{AVAILABLE_FOR_REUSE}\""
    )))
}

/// Compact one table as WiredTiger 11.3 does with `free_space_target`: left
/// alone when less than `target` bytes of it are free, otherwise
/// `WT_SESSION::compact` with no time limit (mongod's `timeout=0`).
///
/// 11.3's foreground compaction checkpoints before it checks the target; a
/// table skipped here is not checkpointed. Tried and measured 2026-10-26:
/// with that checkpoint, a target above the free space freed 256 KB of a
/// file's tail where mongod 8.2.11 frees 0 on the same shape.
pub(crate) fn compact_table(session: &Session, table: &str, target: u64) -> Result<()> {
    if available_bytes(session, table)? < target {
        return Ok(());
    }
    session.compact(table, Some("timeout=0"))?;
    Ok(())
}

/// Whether a WiredTiger error is `EBUSY`.
pub(crate) fn is_busy(e: &WtError) -> bool {
    e.is_busy()
}

impl Storage {
    /// The tables a `compact` of `target` compacts, in mongod's order (the
    /// records, then the indexes): only those that exist.
    pub fn compact_tables(&self, target: CompactTarget<'_>) -> Result<Vec<String>> {
        let mut tables: Vec<String> = match target {
            CompactTarget::Collection(db, coll) => {
                let session = self.conn.open_session()?;
                let descs = self.index_descs(&session, db, coll)?;
                let mut tables = vec![doc_table_for(db, coll), NAT_SEQ_TABLE.to_string()];
                if descs.iter().any(|d| !d.id_claim && d.name != ID_INDEX_NAME) {
                    tables.push(IDX_ENTRIES_TABLE.to_string());
                }
                if descs
                    .iter()
                    .any(|d| d.unique || d.prepare_unique || d.id_claim)
                {
                    tables.push(UNIQ_TABLE.to_string());
                }
                tables
            }
            CompactTarget::Oplog => (0..OPLOG_SHARDS)
                .map(oplog_shard_name)
                .chain(std::iter::once(OPLOG_TABLE.to_string()))
                .collect(),
            CompactTarget::Catalog => vec![COLL_TABLE.to_string()],
        };
        let home = self.home.clone();
        tables.retain(|t| table_file(&home, t).exists());
        Ok(tables)
    }

    /// `compact`: compact the tables `target` lives in, and say how many
    /// bytes their files gave back.
    ///
    /// Safe beside every other operation: WiredTiger compacts a table while
    /// other sessions read and write it. Compaction checkpoints, so it holds
    /// the checkpoint gate a background `validate` reads one checkpoint
    /// under; the periodic checkpoint waits for it, and finds compaction's
    /// own checkpoints already taken. A store held in memory has nothing to
    /// give back and answers 0, as mongod's ephemeral engine does.
    pub fn compact(
        &self,
        target: CompactTarget<'_>,
        dry_run: bool,
        free_space_target_mb: Option<i64>,
    ) -> Result<CompactOutcome> {
        if self.in_memory {
            return Ok(CompactOutcome::Freed(0));
        }
        let key = match target {
            CompactTarget::Collection(db, coll) => format!("{db}.{coll}"),
            CompactTarget::Oplog => "local.oplog.rs".to_string(),
            CompactTarget::Catalog => "system.views".to_string(),
        };
        let Some(_running) = CompactClaim::take(&self.compaction.running, key) else {
            return Ok(CompactOutcome::AlreadyRunning);
        };
        let tables = self.compact_tables(target)?;
        if dry_run {
            return Ok(CompactOutcome::Freed(0));
        }
        let target_bytes = megabytes(free_space_target_mb.unwrap_or(DEFAULT_FREE_SPACE_TARGET_MB));
        let mut before = 0u64;
        for t in &tables {
            before += file_size(&self.home, t)?;
        }
        let session = self.conn.open_session()?;
        {
            let _gate = self.compaction_gate();
            for t in &tables {
                match compact_table(&session, t, target_bytes) {
                    Ok(()) => {}
                    Err(StorageError::Wt(e)) if is_busy(&e) => {
                        return Ok(CompactOutcome::Busy(t.clone()));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        let mut after = 0u64;
        for t in &tables {
            after += file_size(&self.home, t)?;
        }
        Ok(CompactOutcome::Freed(
            (before as i64).saturating_sub(after as i64).max(0),
        ))
    }

    /// Held while WiredTiger compacts, which checkpoints: the periodic
    /// checkpoint and a background `validate` hold it too.
    pub(crate) fn compaction_gate(&self) -> MutexGuard<'_, ()> {
        self.checkpoint_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}

/// The store's compaction state: the targets a `compact` is running on,
/// and the `autoCompact` server, started the first time it is enabled.
#[derive(Default)]
pub(crate) struct Compaction {
    running: Mutex<std::collections::HashSet<String>>,
    auto: std::sync::OnceLock<AutoCompactor>,
}

impl Storage {
    /// `autoCompact`: turn background compaction on (with `runOnce` and
    /// `freeSpaceTargetMB`) or off. See [`crate::auto_compact`].
    pub fn auto_compact(
        &self,
        enable: bool,
        run_once: bool,
        free_space_target_mb: Option<i64>,
    ) -> Result<AutoCompactOutcome> {
        if self.in_memory {
            return Ok(AutoCompactOutcome::InMemory);
        }
        self.compaction
            .auto
            .get_or_init(|| {
                AutoCompactor::new(
                    self.conn.clone(),
                    self.home.clone(),
                    self.checkpoint_gate.clone(),
                )
            })
            .signal(enable, run_once, free_space_target_mb)
    }

    /// The `background-compact` statistics `serverStatus.wiredTiger`
    /// reports: zeros until `autoCompact` has first been enabled.
    pub(crate) fn background_compact_statistics(&self) -> Vec<(String, i64)> {
        match self.compaction.auto.get() {
            Some(a) => a.statistics(),
            None => crate::auto_compact::STATISTICS
                .iter()
                .map(|d| (d.to_string(), 0))
                .collect(),
        }
    }

    /// Stop the `autoCompact` server, if it was ever started: before the
    /// connection closes.
    pub(crate) fn stop_auto_compact(&self) -> Result<()> {
        match self.compaction.auto.get() {
            Some(a) => a.shutdown(),
            None => Ok(()),
        }
    }
}

/// A running `compact`'s claim on its target, released when it ends.
struct CompactClaim<'a> {
    running: &'a Mutex<std::collections::HashSet<String>>,
    key: String,
}

impl<'a> CompactClaim<'a> {
    fn take(running: &'a Mutex<std::collections::HashSet<String>>, key: String) -> Option<Self> {
        let mut set = running.lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(key.clone()) {
            return None;
        }
        Some(CompactClaim { running, key })
    }
}

impl Drop for CompactClaim<'_> {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

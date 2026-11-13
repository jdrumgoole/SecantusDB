//! The spill engine: a second WiredTiger instance that aggregation stages
//! spill to, as mongod 8.2's (RM-325).
//!
//! mongod 8.2 opens it at `<dbpath>/_tmp/spilldb` (`featureFlagCreateSpillKVEngine`,
//! on in 8.2; log line "Opening spill WiredTiger") with no log, its own cache
//! (`spillWiredTigerCacheSizePercentage` 2.5 of the machine's memory, held
//! between `spillWiredTigerCacheSizeMinMB` 100 and `...MaxMB` 1000), one
//! eviction thread, a dirty target of 5% of that cache and dirty / updates
//! triggers of 80%, and drops whatever tables it holds from an earlier run
//! ("Dropping spill idents"). Each spill is a table keyed by record id
//! (`key_format=q,value_format=u`), written in transactions with no
//! timestamps, measured by its file's size as WiredTiger's block manager
//! counts it (`WiredTigerUtil::getIdentSize`: the table's `statistics:` cursor
//! with `statistics=(size)`), and dropped when the stage is done (measured on
//! mongod 8.2.11, 2026-11-13).
//!
//! Opened at the first spill rather than at startup, so a server that never
//! spills (and the PostgreSQL server, which shares this crate) never makes
//! the directory. Its tables are not compressed: mongod's are zstd at level
//! -7 (`spillWiredTigerBlockCompressor`), which the vendored WiredTiger is
//! not built with; that only shows once the engine writes a table's pages to
//! its file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use secantus_wt::{Connection, Session};

use crate::{Result, StorageError};

/// The directory mongod keeps the spill engine in, under its dbpath.
pub const SPILL_DIR: &[&str] = &["_tmp", "spilldb"];

/// A spill table's configuration: mongod's record store over a record-id
/// key, without its zstd compressor (see the module comment).
const TABLE_CFG: &str = "type=file,memory_page_max=10m,split_pct=90,leaf_value_max=64MB,\
                         checksum=on,key_format=q,value_format=u,log=(enabled=false)";

/// How a spill table is dropped: its pages discarded, its file removed.
const DROP_CFG: &str = "force=true";

/// The statistic a table's size on disk is read from.
const FILE_SIZE: &str = "block-manager: file size in bytes";

/// The spill engine's cache, in megabytes, for a machine of `mem_mb`
/// megabytes: 2.5% of it, held between 100 and 1000 (mongod 8.2's
/// `spillWiredTigerCacheSize*` defaults).
pub fn cache_mb(mem_mb: Option<i64>) -> i64 {
    let share = mem_mb.map_or(0, |m| (m as f64 * 0.025) as i64);
    share.clamp(100, 1000)
}

/// The configuration mongod 8.2 opens its spill engine with, for a cache of
/// `cache_mb` megabytes.
pub fn open_config(cache_mb: i64) -> String {
    let pct = |p: i64| cache_mb * p / 100;
    format!(
        "create,cache_size={cache_mb}M,session_max=1024,\
         eviction=(threads_min=1,threads_max=1),eviction_dirty_target={}MB,\
         eviction_dirty_trigger={}MB,eviction_updates_trigger={}MB,config_base=false,\
         statistics=(fast),log=(enabled=false),\
         file_manager=(close_idle_time=600,close_scan_interval=10,close_handle_minimum=2000),\
         statistics_log=(wait=0)",
        pct(5),
        pct(80),
        pct(80)
    )
}

/// The spill engine of the store at `dbpath`, opened by its first table.
pub struct SpillEngine {
    home: PathBuf,
    cache_mb: i64,
    conn: Mutex<Option<Arc<Connection>>>,
    next: AtomicU64,
    /// The engine is in a directory of its own under the system's temporary
    /// directory (a store with no files of its own), removed with it.
    remove_home: bool,
}

impl SpillEngine {
    /// The spill engine for the store at `dbpath`, with a cache of
    /// `cache_mb` megabytes. Nothing is opened yet.
    pub fn new(dbpath: &Path, cache_mb: i64) -> SpillEngine {
        let mut home = dbpath.to_path_buf();
        for part in SPILL_DIR {
            home.push(part);
        }
        SpillEngine {
            home,
            cache_mb,
            conn: Mutex::new(None),
            next: AtomicU64::new(0),
            remove_home: false,
        }
    }

    /// The spill engine for a store that keeps no files (`in_memory=true`):
    /// in a directory of its own under the system's temporary directory, as
    /// such a store's index builds spill there, removed when the engine is.
    pub fn temporary(cache_mb: i64) -> SpillEngine {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let home = std::env::temp_dir().join(format!(
            "secantus-spilldb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        SpillEngine {
            home,
            cache_mb,
            conn: Mutex::new(None),
            next: AtomicU64::new(0),
            remove_home: true,
        }
    }

    /// Where the engine keeps its files.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The open engine, opening it (and dropping what an earlier run left in
    /// it) the first time.
    fn connection(&self) -> Result<Arc<Connection>> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = conn.as_ref() {
            return Ok(Arc::clone(c));
        }
        std::fs::create_dir_all(&self.home).map_err(|e| {
            StorageError::Internal(format!(
                "could not create the spill engine's directory {}: {e}",
                self.home.display()
            ))
        })?;
        let home = self.home.to_str().ok_or_else(|| {
            StorageError::Internal(format!(
                "the spill engine's directory {} is not UTF-8",
                self.home.display()
            ))
        })?;
        let opened = Arc::new(Connection::open(home, &open_config(self.cache_mb))?);
        drop_all(&opened.open_session()?)?;
        *conn = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// A new, empty spill table.
    pub fn create_table(&self) -> Result<SpillTable> {
        let conn = self.connection()?;
        let session = conn.open_session()?;
        let uri = format!(
            "table:internal-spill-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::Relaxed)
        );
        session.create(&uri, TABLE_CFG)?;
        Ok(SpillTable {
            session: Some(session),
            uri,
            _conn: conn,
        })
    }
}

impl Drop for SpillEngine {
    fn drop(&mut self) {
        let conn = self
            .conn
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if !self.remove_home {
            return;
        }
        // Every table holds the connection; the last one closes it.
        let closed = conn.is_none_or(|c| Arc::into_inner(c).is_some());
        if !closed {
            eprintln!(
                "secantus: the spill engine at {} is still in use; its directory is left",
                self.home.display()
            );
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&self.home) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "secantus: could not remove the spill engine's directory {}: {e}",
                    self.home.display()
                );
            }
        }
    }
}

/// Drop every table in the engine: mongod's "Dropping spill idents" at
/// startup, here when the engine opens.
fn drop_all(session: &Session) -> Result<()> {
    let mut tables = Vec::new();
    {
        let cursor = session.open_cursor("metadata:", None)?;
        while cursor.next()? {
            let key = cursor.get_key_s()?;
            if key.starts_with("table:") {
                tables.push(key);
            }
        }
    }
    for t in tables {
        session.drop_table(&t, Some(DROP_CFG))?;
    }
    Ok(())
}

/// One spill table. Dropped by [`SpillTable::drop_table`], or when the value
/// is, where an error can only be printed.
pub struct SpillTable {
    session: Option<Session>,
    uri: String,
    /// Keeps the engine open while the table is.
    _conn: Arc<Connection>,
}

impl SpillTable {
    fn session(&self) -> &Session {
        self.session.as_ref().expect("a table not yet dropped")
    }

    /// Write `records` (record id, bytes) in one transaction.
    pub fn insert(&mut self, records: &[(i64, Vec<u8>)]) -> Result<()> {
        let session = self.session();
        session.begin_transaction(None)?;
        let written = (|| -> Result<()> {
            let cursor = session.open_cursor(&self.uri, None)?;
            for (id, bytes) in records {
                cursor.set_key_q(*id);
                cursor.set_value_u(bytes);
                cursor.insert()?;
            }
            Ok(())
        })();
        match written {
            Ok(()) => session.commit_transaction(None).map_err(Into::into),
            Err(e) => {
                session.rollback_transaction(None)?;
                Err(e)
            }
        }
    }

    /// The table's file size, as WiredTiger's block manager counts it.
    pub fn storage_size(&self) -> Result<i64> {
        let cursor = self.session().open_cursor(
            &format!("statistics:{}", self.uri),
            Some("statistics=(size)"),
        )?;
        while cursor.next()? {
            let (description, value) = cursor.get_value_stat()?;
            if description == FILE_SIZE {
                return Ok(value);
            }
        }
        Err(StorageError::Internal(format!(
            "{}'s statistics have no \"{FILE_SIZE}\"",
            self.uri
        )))
    }

    /// Drop the table and its file.
    pub fn drop_table(mut self) -> Result<()> {
        self.close()
    }

    fn close(&mut self) -> Result<()> {
        // `force`: the tree is dead, so WiredTiger discards its pages
        // instead of refusing (`EBUSY`) to close a tree whose writes no
        // checkpoint has taken.
        match self.session.take() {
            Some(session) => {
                session.drop_table(&self.uri, Some(DROP_CFG))?;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl Drop for SpillTable {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("secantus: could not drop spill table {}: {e}", self.uri);
        }
    }
}

impl secantus_core::spill_table::SpillTables for SpillEngine {
    fn create(
        &self,
    ) -> std::result::Result<Box<dyn secantus_core::spill_table::SpillTable>, String> {
        self.create_table()
            .map(|t| Box::new(t) as Box<dyn secantus_core::spill_table::SpillTable>)
            .map_err(|e| e.to_string())
    }
}

impl secantus_core::spill_table::SpillTable for SpillTable {
    fn insert(&mut self, records: &[(i64, Vec<u8>)]) -> std::result::Result<(), String> {
        SpillTable::insert(self, records).map_err(|e| e.to_string())
    }

    fn storage_size(&self) -> std::result::Result<i64, String> {
        SpillTable::storage_size(self).map_err(|e| e.to_string())
    }

    fn drop_table(self: Box<Self>) -> std::result::Result<(), String> {
        SpillTable::drop_table(*self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_a_fortieth_of_memory_between_100_and_1000_mb() {
        // 32 GB: mongod 8.2.11 opened its spill engine with cache_size=819M.
        assert_eq!(cache_mb(Some(32_768)), 819);
        assert_eq!(cache_mb(Some(1_024)), 100);
        assert_eq!(cache_mb(Some(1_048_576)), 1000);
        assert_eq!(cache_mb(None), 100);
        let cfg = open_config(819);
        assert!(cfg.contains("cache_size=819M"), "{cfg}");
        assert!(cfg.contains("eviction_dirty_target=40MB"), "{cfg}");
        assert!(cfg.contains("eviction_dirty_trigger=655MB"), "{cfg}");
    }

    /// A table the engine holds in its cache keeps the file WiredTiger made
    /// it with, 4096 bytes, as mongod 8.2.11's window spill of 300 documents
    /// of 1000 bytes did; dropping it removes the file.
    #[test]
    fn a_spill_table_held_in_cache_is_4096_bytes_and_drops_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SpillEngine::new(dir.path(), 100);
        let mut table = engine.create_table().unwrap();
        let records: Vec<(i64, Vec<u8>)> = (1..=300).map(|i| (i, vec![b'x'; 1050])).collect();
        table.insert(&records).unwrap();
        assert_eq!(table.storage_size().unwrap(), 4096);
        let files = || {
            std::fs::read_dir(engine.home())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("internal-spill-")
                })
                .count()
        };
        assert_eq!(files(), 1);
        table.drop_table().unwrap();
        assert_eq!(files(), 0);
    }

    /// A store with no files spills under the system's temporary
    /// directory, and the engine's directory goes with the engine.
    #[test]
    fn a_temporary_engine_removes_its_directory() {
        let engine = SpillEngine::temporary(100);
        let home = engine.home().to_path_buf();
        let mut table = engine.create_table().unwrap();
        table.insert(&[(1, vec![1u8; 10])]).unwrap();
        assert!(home.is_dir());
        table.drop_table().unwrap();
        drop(engine);
        assert!(!home.exists());
    }

    /// An engine opened over tables an earlier run left drops them.
    #[test]
    fn opening_drops_what_an_earlier_run_left() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SpillEngine::new(dir.path(), 100);
        {
            std::fs::create_dir_all(engine.home()).unwrap();
            let earlier =
                Connection::open(engine.home().to_str().unwrap(), &open_config(100)).unwrap();
            let session = earlier.open_session().unwrap();
            session.create("table:internal-left", TABLE_CFG).unwrap();
        }
        let _t = engine.create_table().unwrap();
        let session = engine.connection().unwrap().open_session().unwrap();
        let cursor = session.open_cursor("metadata:", None).unwrap();
        let mut tables = 0;
        while cursor.next().unwrap() {
            if cursor.get_key_s().unwrap().starts_with("table:") {
                tables += 1;
            }
        }
        assert_eq!(tables, 1);
    }
}

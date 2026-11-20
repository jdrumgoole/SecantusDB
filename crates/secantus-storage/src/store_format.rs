//! The store's on-disk format: what a store directory must hold before
//! WiredTiger is opened on it, and the refusal of a store in an older layout.
//!
//! SecantusDB stores are moving to mongod's own layout
//! (`tasks/mongod-storage-model-plan.md`), so a mongod 8.2 can open them and
//! SecantusDB can open mongod's. A store therefore carries mongod's
//! `storage.bson`, keeps WiredTiger's log in `journal/` (snappy-compressed,
//! as mongod's), and is written by a WiredTiger built as mongod builds it
//! (`WT_STANDALONE_BUILD=OFF`).
//!
//! That WiredTiger refuses the files of the earlier layout outright (their
//! btree version is 2.1, it reads only 1.1; it answers `EACCES`). So the
//! layout is checked HERE, from the directory listing, before
//! `wiredtiger_open`: an old store gets a message naming what to do, never
//! WiredTiger's error.
//!
//! [`FORMAT_FILE`] records the layout version of a store SecantusDB wrote.
//! mongod ignores the file. A store without it but with `storage.bson` was
//! written by mongod.

use std::fs;
use std::io::Write;
use std::path::Path;

use bson::doc;

use crate::{Result, StorageError};

/// The layout version this build writes and reads. Every change to the
/// on-disk layout moves it on (each phase of the storage-model plan does),
/// and a store at any other version is refused.
pub const STORE_FORMAT_VERSION: u32 = 4;

/// The file holding [`STORE_FORMAT_VERSION`] in a SecantusDB-written store.
pub const FORMAT_FILE: &str = "secantus.format";

/// mongod's `storage.bson` file name.
pub const STORAGE_BSON: &str = "storage.bson";

/// The directory WiredTiger keeps its log in, as mongod's `--dbpath` does.
pub const JOURNAL_DIR: &str = "journal";

/// The last releases that read the earlier layout, named in the refusal.
const LAST_OLD_LAYOUT_RELEASES: &str = "secantusd-mdb 0.5.3-beta.198 or secantusd-pg 0.1.0-beta.20";

/// The bytes of mongod 8.2.11's `storage.bson` (measured in the plan's phase
/// 0b): `{storage: {engine: "wiredTiger", options: {directoryPerDB: false,
/// directoryForIndexes: false, groupCollections: false}}}`.
pub fn mongod_storage_bson() -> Vec<u8> {
    let d = doc! {
        "storage": {
            "engine": "wiredTiger",
            "options": {
                "directoryPerDB": false,
                "directoryForIndexes": false,
                "groupCollections": false,
            },
        },
    };
    secantus_core::bsondoc::to_vec(&d).expect("a fixed document always encodes")
}

/// What [`prepare`] found at `home`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// No store yet: [`prepare`] laid out a new one.
    Created,
    /// A store this build wrote, at [`STORE_FORMAT_VERSION`].
    Current,
}

/// Check the store at `home` before WiredTiger opens it, and lay out a new
/// one: `journal/`, `storage.bson` and [`FORMAT_FILE`].
///
/// Refuses, with what to do, a store in the earlier SecantusDB layout, one
/// at another layout version, and (until SecantusDB reads mongod's catalog,
/// backlog item RM-723) one mongod wrote.
pub fn prepare(home: &Path) -> Result<StoreKind> {
    let has_wt = home.join("WiredTiger.wt").exists();
    let has_storage_bson = home.join(STORAGE_BSON).exists();
    let format = home.join(FORMAT_FILE);

    if !has_wt {
        if has_storage_bson && !format.exists() {
            // mongod writes storage.bson before WiredTiger's files; a
            // directory holding only that is still mongod's.
            return Err(mongod_store(home));
        }
        fs::create_dir_all(home.join(JOURNAL_DIR)).map_err(|e| io_error(home, e))?;
        if !has_storage_bson {
            write_atomically(&home.join(STORAGE_BSON), &mongod_storage_bson())?;
        }
        write_atomically(&format, format!("{STORE_FORMAT_VERSION}\n").as_bytes())?;
        return Ok(StoreKind::Created);
    }

    if !has_storage_bson {
        return Err(StorageError::Internal(format!(
            "the store at {} was written by an earlier SecantusDB, whose on-disk layout \
             this build does not read (it moved to mongod's layout). Dump it with the \
             release that wrote it ({LAST_OLD_LAYOUT_RELEASES} or earlier): \
             `mongodump --uri <that server>` / `pg_dump`, then restore into a new store \
             with this build (`mongorestore` / `psql`).",
            home.display()
        )));
    }
    if !format.exists() {
        return Err(mongod_store(home));
    }
    let text = fs::read_to_string(&format).map_err(|e| io_error(home, e))?;
    match text.trim().parse::<u32>() {
        Ok(STORE_FORMAT_VERSION) => {
            fs::create_dir_all(home.join(JOURNAL_DIR)).map_err(|e| io_error(home, e))?;
            Ok(StoreKind::Current)
        }
        Ok(other) => Err(StorageError::Internal(format!(
            "the store at {} is in SecantusDB store format {other}; this build reads \
             format {STORE_FORMAT_VERSION} only. Dump it with the build that wrote it, \
             then restore into a new store with this one.",
            home.display()
        ))),
        Err(_) => Err(StorageError::Internal(format!(
            "{} does not hold a SecantusDB store format version: {:?}",
            format.display(),
            text.trim()
        ))),
    }
}

/// The WiredTiger connection config with the log moved to `journal/` and
/// snappy-compressed, as mongod opens it. Every `log=(...)` clause gains
/// `path` and `compressor` unless it already names them; a config without
/// one gets a logging clause of its own. An in-memory config is returned
/// unchanged: it has no log.
pub fn mongod_log_config(config: &str) -> String {
    if config.contains("in_memory=true") {
        return config.to_string();
    }
    let mut out = String::with_capacity(config.len() + 48);
    let mut rest = config;
    let mut found = false;
    while let Some(at) = rest.find("log=(") {
        // `statistics_log=(` and the like are not the log clause.
        let preceded_by_ident = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let open = at + "log=(".len();
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        if preceded_by_ident {
            continue;
        }
        found = true;
        let close = rest.find(')').unwrap_or(rest.len());
        let body = &rest[..close];
        let mut extra = String::new();
        if !body
            .split(',')
            .any(|kv| kv.trim_start().starts_with("path="))
        {
            extra.push_str("path=journal,");
        }
        if !body
            .split(',')
            .any(|kv| kv.trim_start().starts_with("compressor="))
        {
            extra.push_str("compressor=snappy,");
        }
        if body.is_empty() {
            extra.pop();
        }
        out.push_str(&extra);
    }
    out.push_str(rest);
    if !found {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str("log=(enabled=true,path=journal,compressor=snappy)");
    }
    out
}

fn mongod_store(home: &Path) -> StorageError {
    StorageError::Internal(format!(
        "the store at {} was written by mongod. Opening mongod's stores is not \
         supported yet (backlog item RM-723); move the data with mongodump / \
         mongorestore.",
        home.display()
    ))
}

fn io_error(home: &Path, e: std::io::Error) -> StorageError {
    StorageError::Internal(format!("preparing the store at {}: {e}", home.display()))
}

/// Write `bytes` to `path` through a temporary file and a rename, and sync
/// both, so a crash leaves the old file or the new one, never part of one.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let home = path.parent().unwrap_or_else(|| Path::new("."));
    let run = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = fs::File::open(home) {
            // Persists the rename; not every platform can sync a directory.
            let _ = dir.sync_all();
        }
        Ok(())
    };
    run().map_err(|e| io_error(home, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_bson_is_mongods() {
        // 114 bytes, as mongod 8.2.11 writes it (phase 0b fixture).
        let bytes = mongod_storage_bson();
        assert_eq!(bytes.len(), 114);
        assert_eq!(&bytes[..4], &[0x72, 0, 0, 0]);
    }

    #[test]
    fn log_config_gains_journal_and_snappy() {
        assert_eq!(
            mongod_log_config("create,log=(enabled=true,file_max=128MB,prealloc=false),statistics=(fast)"),
            "create,log=(path=journal,compressor=snappy,enabled=true,file_max=128MB,prealloc=false),statistics=(fast)"
        );
        // Already named: left alone.
        assert_eq!(
            mongod_log_config("log=(enabled=true,path=elsewhere,compressor=zstd)"),
            "log=(enabled=true,path=elsewhere,compressor=zstd)"
        );
        // statistics_log is not the log clause.
        assert_eq!(
            mongod_log_config("statistics_log=(wait=0),log=(enabled=true)"),
            "statistics_log=(wait=0),log=(path=journal,compressor=snappy,enabled=true)"
        );
        assert_eq!(
            mongod_log_config("create"),
            "create,log=(enabled=true,path=journal,compressor=snappy)"
        );
        assert_eq!(
            mongod_log_config("create,in_memory=true"),
            "create,in_memory=true"
        );
    }

    #[test]
    fn a_new_store_is_laid_out_as_mongods() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(prepare(dir.path()).unwrap(), StoreKind::Created);
        assert!(dir.path().join(JOURNAL_DIR).is_dir());
        assert_eq!(
            fs::read(dir.path().join(STORAGE_BSON)).unwrap(),
            mongod_storage_bson()
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(FORMAT_FILE)).unwrap(),
            format!("{STORE_FORMAT_VERSION}\n")
        );
    }

    #[test]
    fn an_old_layout_store_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
        let err = prepare(dir.path()).unwrap_err().to_string();
        assert!(err.contains("earlier SecantusDB"), "{err}");
        assert!(err.contains("0.5.3-beta.198"), "{err}");
    }

    #[test]
    fn a_mongod_store_is_refused_for_now() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
        fs::write(dir.path().join(STORAGE_BSON), mongod_storage_bson()).unwrap();
        let err = prepare(dir.path()).unwrap_err().to_string();
        assert!(err.contains("written by mongod"), "{err}");
    }

    #[test]
    fn another_format_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
        fs::write(dir.path().join(STORAGE_BSON), mongod_storage_bson()).unwrap();
        fs::write(dir.path().join(FORMAT_FILE), "0\n").unwrap();
        let err = prepare(dir.path()).unwrap_err().to_string();
        assert!(err.contains("store format 0"), "{err}");
    }

    #[test]
    fn the_previous_layout_is_refused_and_this_one_is_format_4() {
        // Format 3 kept the `_id` index and the unique claims in tables
        // shared by every collection; format 4 gives `_id_` a table of its
        // own, in KeyString, and holds uniqueness in the index tables.
        assert_eq!(STORE_FORMAT_VERSION, 4);
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
        fs::write(dir.path().join(STORAGE_BSON), mongod_storage_bson()).unwrap();
        fs::write(dir.path().join(FORMAT_FILE), "3\n").unwrap();
        let err = prepare(dir.path()).unwrap_err().to_string();
        assert!(err.contains("store format 3"), "{err}");
        assert!(err.contains("format 4 only"), "{err}");
    }
}

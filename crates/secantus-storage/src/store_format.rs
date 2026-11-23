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
pub const STORE_FORMAT_VERSION: u32 = 11;

/// The file holding [`STORE_FORMAT_VERSION`] in a SecantusDB-written store.
pub const FORMAT_FILE: &str = "secantus.format";

/// mongod's `storage.bson` file name.
pub const STORAGE_BSON: &str = "storage.bson";

/// mongod's lock file in its `--dbpath`.
const MONGOD_LOCK: &str = "mongod.lock";

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
    /// A store mongod wrote (`storage.bson`, no [`FORMAT_FILE`]) holding
    /// nothing [`catalog_findings`] refuses, or one whose log needs
    /// recovery first (a killed mongod), which is checked once WiredTiger
    /// has recovered it. The open builds this server's registry from
    /// mongod's catalog, and [`mark_opened`] writes the format file once
    /// it has succeeded (mongod ignores the file).
    Mongod,
}

/// Check the store at `home` before WiredTiger opens it, and lay out a new
/// one: `journal/`, `storage.bson` and [`FORMAT_FILE`].
///
/// Refuses, with what to do, a store in the earlier SecantusDB layout, one
/// at another layout version, and one mongod wrote that holds something
/// this server cannot serve, named ([`catalog_findings`]).
pub fn prepare(home: &Path) -> Result<StoreKind> {
    let has_wt = home.join("WiredTiger.wt").exists();
    let has_storage_bson = home.join(STORAGE_BSON).exists();
    let format = home.join(FORMAT_FILE);

    if !has_wt {
        if has_storage_bson && !format.exists() {
            // mongod writes storage.bson before WiredTiger's files; a
            // directory holding only that is still mongod's, and holds no
            // store to open.
            return Err(mongod_store(home, &[]));
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
        // mongod empties its lock file as it stops cleanly and leaves its
        // pid in it otherwise (its "Detected unclean shutdown"): a store it
        // was killed on needs log recovery, which read-only WiredTiger
        // refuses, so it is checked once it has been opened and recovered.
        let unclean = fs::metadata(home.join(MONGOD_LOCK)).is_ok_and(|m| m.len() > 0);
        if unclean {
            return Ok(StoreKind::Mongod);
        }
        return match mongod_store_findings(home) {
            Ok(findings) if !findings.is_empty() => Err(mongod_store(home, &findings)),
            // Nothing refused; or a log WiredTiger must recover before the
            // catalog can be read, checked again once it has.
            _ => Ok(StoreKind::Mongod),
        };
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

/// The refusal of a store mongod wrote, naming what in it this server
/// cannot serve.
pub(crate) fn mongod_store(home: &Path, findings: &[String]) -> StorageError {
    let what = if findings.is_empty() {
        " It holds no WiredTiger store yet.".to_string()
    } else {
        format!(
            " It holds what this server cannot serve: {}.",
            findings.join("; ")
        )
    };
    StorageError::Internal(format!(
        "the store at {} was written by mongod.{what} Move the data with mongodump / \
         mongorestore.",
        home.display()
    ))
}

/// The refusal of a store this server wrote and mongod wrote to since,
/// naming what mongod added that this server cannot serve.
pub(crate) fn written_by_mongod_since(home: &Path, findings: &[String]) -> StorageError {
    StorageError::Internal(format!(
        "the store at {} was written by mongod since this server last closed it, and \
         holds what this server cannot serve: {}. Open it with mongod, or move the data \
         with mongodump / mongorestore.",
        home.display(),
        findings.join("; ")
    ))
}

/// Write [`FORMAT_FILE`] into a store mongod wrote, once this server has
/// opened it: from then on it is a store of this format.
pub fn mark_opened(home: &Path) -> Result<()> {
    write_atomically(
        &home.join(FORMAT_FILE),
        format!("{STORE_FORMAT_VERSION}\n").as_bytes(),
    )
}

/// The index kinds whose keys this server does not write as mongod does
/// (storage-model plan, phase 4; mongod's own `2d`, `2dsphere` and wildcard indexes are
/// refused until a store of its is adopted with them, not for their keys, which are
/// the ones this server writes since format 11), by the value naming
/// them in an index's key pattern.
const UNSERVED_INDEX_KINDS: &[&str] = &["2d", "2dsphere", "2dsphere_bucket", "text", "columnstore"];

/// The collection options of a `_mdb_catalog` entry this server keeps and
/// serves: mongod's own (`uuid`, the oplog's `autoIndexId`), what `create`
/// and `collMod` persist here, and a clustered or time-series collection's.
const SERVED_COLLECTION_OPTIONS: &[&str] = &[
    "uuid",
    "autoIndexId",
    "capped",
    "size",
    "max",
    "clusteredIndex",
    "expireAfterSeconds",
    "validator",
    "validationLevel",
    "validationAction",
    "changeStreamPreAndPostImages",
    "timeseries",
    "collation",
    "storageEngine",
    "indexOptionDefaults",
];

/// The fields of an index spec this server keeps and serves.
const SERVED_INDEX_OPTIONS: &[&str] = &[
    "v",
    "key",
    "name",
    "unique",
    "sparse",
    "partialFilterExpression",
    "expireAfterSeconds",
    "collation",
    "hidden",
    "2dsphereIndexVersion",
    "textIndexVersion",
    "weights",
    "default_language",
    "language_override",
    "bits",
    "min",
    "max",
    "wildcardProjection",
];

/// What in a store mongod wrote this server cannot serve, named (plan
/// decision 10: refused by name when the store is opened, never skipped),
/// read without writing anything: WiredTiger is opened read-only, which it
/// refuses for a store whose log still needs recovery (a mongod that was
/// killed): `Err` with WiredTiger's answer then, and the store is checked
/// again once it has been opened and recovered ([`catalog_findings`]).
pub fn mongod_store_findings(home: &Path) -> std::result::Result<Vec<String>, String> {
    let config = mongod_log_config("readonly=true,cache_size=64M,log=(enabled=true)");
    let conn = secantus_wt::Connection::open(&home.to_string_lossy(), &config)
        .map_err(|e| e.to_string())?;
    let found = conn
        .open_session()
        .map_err(StorageError::from)
        .and_then(|session| catalog_findings(&session));
    Ok(match found {
        Ok(found) => found,
        Err(e) => vec![format!("a catalog this server could not read ({e})")],
    })
}

/// What in the store `session` reads this server cannot serve, named: a
/// replica-set configuration of more than one member, an index of a kind
/// this server does not key as mongod does, an index of another version
/// than 2 or one whose build has not finished, a collection or index option
/// it does not keep, a time-series collection (RM-754), a feature
/// compatibility version other than 8.2, users and roles (RM-744). Checked
/// whenever another server (mongod) wrote the store, before this server
/// writes to it.
pub(crate) fn catalog_findings(session: &secantus_wt::Session) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut idents: Vec<(String, String)> = Vec::new();
    let cur = session.open_cursor("table:_mdb_catalog", None)?;
    let mut more = cur.next()?;
    while more {
        let entry = secantus_core::bsondoc::from_bytes(&cur.get_value_u()?)
            .map_err(|e| StorageError::Internal(format!("_mdb_catalog: {e}")))?;
        more = cur.next()?;
        let Ok(md) = entry.get_document("md") else {
            continue;
        };
        let ns = md.get_str("ns").unwrap_or("").to_string();
        if let Ok(ident) = entry.get_str("ident") {
            idents.push((ns.clone(), ident.to_string()));
        }
        if ns
            .split_once('.')
            .is_some_and(|(_, c)| c.starts_with(secantus_core::viewdefs::BUCKETS_PREFIX))
        {
            out.push(format!(
                "{ns}, a time-series collection's buckets, which this server does not open \
                 yet (RM-754)"
            ));
            continue;
        }
        if let Ok(options) = md.get_document("options") {
            for (option, _) in options {
                if !SERVED_COLLECTION_OPTIONS.contains(&option.as_str()) {
                    out.push(format!("{ns}'s collection option {option}"));
                }
            }
        }
        for ix in md.get_array("indexes").into_iter().flatten() {
            let Some(ix) = ix.as_document() else {
                continue;
            };
            let Ok(spec) = ix.get_document("spec") else {
                continue;
            };
            let name = spec.get_str("name").unwrap_or("");
            if ix.get_bool("ready") == Ok(false) {
                out.push(format!(
                    "{ns}'s index {name}, whose build has not finished (this server does not \
                     resume mongod's index builds yet, RM-755)"
                ));
            }
            let version = spec.get("v").and_then(|v| match v {
                bson::Bson::Int32(n) => Some(i64::from(*n)),
                bson::Bson::Int64(n) => Some(*n),
                bson::Bson::Double(d) => Some(*d as i64),
                _ => None,
            });
            if version != Some(2) {
                out.push(format!(
                    "{ns}'s index {name} of version {:?} (this server keys version 2)",
                    spec.get("v")
                ));
            }
            let Ok(key) = spec.get_document("key") else {
                continue;
            };
            let kind = key.iter().find_map(|(field, v)| {
                if field == "$**" || field.ends_with(".$**") {
                    return Some("wildcard".to_string());
                }
                v.as_str()
                    .filter(|k| UNSERVED_INDEX_KINDS.contains(k))
                    .map(str::to_string)
            });
            if let Some(kind) = kind {
                out.push(format!("{ns}'s index {name} ({kind})"));
            }
            for (option, _) in spec {
                if !SERVED_INDEX_OPTIONS.contains(&option.as_str()) {
                    out.push(format!("{ns}'s index {name}'s option {option}"));
                }
            }
        }
    }
    let docs = |ns: &str| -> Result<Vec<bson::Document>> {
        let Some((_, ident)) = idents.iter().find(|(n, _)| n == ns) else {
            return Ok(Vec::new());
        };
        let cur = session.open_cursor(&format!("table:{ident}"), None)?;
        let mut docs = Vec::new();
        let mut more = cur.next()?;
        while more {
            if let Ok(d) = secantus_core::bsondoc::from_bytes(&cur.get_value_u()?) {
                docs.push(d);
            }
            more = cur.next()?;
        }
        Ok(docs)
    };
    for config in docs("local.system.replset")? {
        let members = config.get_array("members").map_or(0, Vec::len);
        if members != 1 {
            out.push(format!(
                "a local.system.replset configuration of {members} members (set {:?}); a \
                 single-node server serves a set of one",
                config.get_str("_id").unwrap_or("")
            ));
        }
    }
    for v in docs("admin.system.version")? {
        if v.get_str("_id") == Ok("featureCompatibilityVersion")
            && (v.get_str("version") != Ok("8.2") || v.contains_key("targetVersion"))
        {
            out.push(format!(
                "a featureCompatibilityVersion of {:?} (this server serves 8.2)",
                v.get_str("version").unwrap_or("")
            ));
        }
    }
    for ns in ["admin.system.users", "admin.system.roles"] {
        let n = docs(ns)?.len();
        if n > 0 {
            out.push(format!(
                "{n} documents of {ns}, which this server does not read (RM-744)"
            ));
        }
    }
    for (ns, _) in &idents {
        let Some((db, "system.views")) = ns.split_once('.') else {
            continue;
        };
        for view in docs(ns)? {
            let unpacks = view
                .get_array("pipeline")
                .ok()
                .and_then(|p| p.first())
                .and_then(|s| {
                    s.as_document()
                        .map(|s| s.contains_key("$_internalUnpackBucket"))
                });
            if unpacks == Some(true) {
                out.push(format!(
                    "{}, a time-series collection of {db}, which this server does not open \
                     yet (RM-754)",
                    view.get_str("_id").unwrap_or("")
                ));
            }
        }
    }
    Ok(out)
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
    fn a_mongod_store_is_opened_and_gets_no_format_file_before_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
        fs::write(dir.path().join(STORAGE_BSON), mongod_storage_bson()).unwrap();
        // Read-only WiredTiger cannot read it: checked again once opened.
        assert_eq!(prepare(dir.path()).unwrap(), StoreKind::Mongod);
        assert!(!dir.path().join(FORMAT_FILE).exists());
        // A directory with mongod's storage.bson and nothing else.
        let dir = tempfile::tempdir().unwrap();
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
    fn the_previous_layouts_are_refused_and_this_one_is_format_11() {
        // Format 10 keyed a `2d` index by a cell of its own (BinData subtype
        // 0, y first), a `2dsphere` index by a covering of every ancestor of
        // each cell (BinData) and a wildcard index by one null entry per
        // document; format 11 writes mongod's keys for all three.
        // Format 9 kept a drop's and a rename's pending work in
        // `secantus_drop_tombstones` and finished it at the next open; format
        // 10 does both in one transaction over the catalog, as mongod does,
        // and the table is gone.
        // Format 8 kept a view (and a time-series collection's own
        // namespace) as a collection with tables and an `_mdb_catalog` row,
        // and a multikey index held the whole leaf array as an entry besides
        // its elements; format 9 keeps a view as a `system.views` document
        // and an index holds mongod's keys only.
        // Format 7 kept the collection sizes in `secantus_size_storer`, keyed
        // by namespace (`key_format=SS`); format 8 keeps them in mongod's
        // `sizeStorer`, keyed by the ident's table URI, with only the index
        // bytes and the clean-close marker in this server's own table.
        // Format 6 held a document in its table as `[id_key length][id_key]
        // [BSON]`; format 7 stores the document alone, as mongod does.
        assert_eq!(STORE_FORMAT_VERSION, 11);
        for old in ["4", "5", "6", "7", "8", "9", "10"] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("WiredTiger.wt"), b"").unwrap();
            fs::write(dir.path().join(STORAGE_BSON), mongod_storage_bson()).unwrap();
            fs::write(dir.path().join(FORMAT_FILE), format!("{old}\n")).unwrap();
            let err = prepare(dir.path()).unwrap_err().to_string();
            assert!(err.contains(&format!("store format {old}")), "{err}");
            assert!(err.contains("format 11 only"), "{err}");
        }
    }
}

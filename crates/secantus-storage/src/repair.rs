//! `secantusd-rs --repair`: mongod's `--repair`, over this store's layout.
//!
//! mongod 8.2.11 `--repair` (measured 2026-10-26) opens the store, and for
//! each table runs WiredTiger's `verify`; a table that fails it is salvaged
//! (`WT_SESSION::salvage`: the file rebuilt from whatever of it still reads
//! back, the rest discarded). Then it validates every collection against its
//! indexes, rebuilding the indexes of a collection whose records were
//! salvaged or whose indexes disagree with them, and exits 0. Its log names
//! each step: `Verify succeeded. Not salvaging.` / `Verify failed. Running a
//! salvage operation.` for a table, `Repairing collection` and then either
//! `Validate did not make any repairs` or an index build for a collection.
//!
//! Here the tables are shared (the document shards, the `_id` table, the
//! entries and claims tables, the catalog, the oplog), so:
//!
//! 1. Every table is verified, and salvaged when that fails, on a bare
//!    WiredTiger connection before the store is opened -- `verify` and
//!    `salvage` need a table nobody else has open, and opening the store
//!    reads its tables. A store whose WiredTiger metadata itself is damaged
//!    (`WT_TRY_SALVAGE` at open) is opened with `salvage=true` first.
//! 2. The store is opened, which replays the oplog as after a crash.
//! 3. Every collection is checked against its indexes; a collection whose
//!    document shard was salvaged, or whose indexes were in a salvaged
//!    table, or whose check finds an entry missing or extra, has its `_id`
//!    index and its other indexes rebuilt from its documents.
//! 4. When the claims table was salvaged, every `_id` claim is dropped and
//!    taken again from the documents as the store reopens.
//!
//! Two documents a salvage brought back with the same `_id` (a deleted one
//! resurrected beside its replacement) keep the earlier one in the `_id`
//! index; mongod moves the other to `local.lost_and_found`, which this
//! server does not have, so here it stays where it was and the repair says
//! how many there were.

use super::*;

/// What a repair found and did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// Tables that passed `verify`.
    pub verified: Vec<String>,
    /// Tables that failed it and were salvaged.
    pub salvaged: Vec<String>,
    /// Whether WiredTiger's own metadata had to be salvaged to open.
    pub metadata_salvaged: bool,
    /// Collections whose indexes were rebuilt, as `db.coll`.
    pub rebuilt: Vec<String>,
    /// Documents left out of the `_id` index for sharing an `_id` with an
    /// earlier one.
    pub duplicate_ids: u64,
}

/// Repair the store at `home`, opened with `opts` (as the server would open
/// it), reporting each step to `log`. The store is closed again on return.
pub fn repair(
    home: &str,
    opts: &StorageOptions,
    log: &mut dyn FnMut(&str),
) -> Result<RepairReport> {
    if !std::path::Path::new(home).is_dir() {
        return Err(StorageError::Internal(format!(
            "Data directory {home} not found. Create the missing directory or specify another \
             path using --storage-path"
        )));
    }
    let mut report = RepairReport::default();
    let config = opts
        .wt_config
        .clone()
        .unwrap_or_else(|| DEFAULT_CONFIG.to_string());
    verify_and_salvage(home, &config, &mut report, log)?;

    let storage = Storage::open_with_options(home, opts)?;
    let salvaged = |t: &str| report.salvaged.iter().any(|s| s == t);
    let shared_index_salvaged = [NAT_SEQ_TABLE, IDX_ENTRIES_TABLE, UNIQ_TABLE, IDX_TABLE]
        .iter()
        .any(|t| salvaged(t));
    let mut rebuilt = Vec::new();
    let mut duplicates = 0u64;
    for db in storage.list_databases()? {
        for coll in storage.list_collections(&db)? {
            let ns = format!("{db}.{coll}");
            log(&format!("Repairing collection {ns}"));
            let check = storage.check_collection(&db, &coll)?;
            let inconsistent = check_disagrees(&check);
            if shared_index_salvaged || salvaged(&doc_table_for(&db, &coll)) || inconsistent {
                duplicates += storage.rebuild_indexes(&db, &coll)?;
                log(&format!("Rebuilt the indexes of {ns}"));
                rebuilt.push(ns);
            } else {
                log(&format!("Validate did not make any repairs: {ns}"));
            }
        }
    }
    if duplicates > 0 {
        log(&format!(
            "{duplicates} documents share an _id with an earlier document and are not in the \
             _id index"
        ));
    }
    let claims_salvaged = salvaged(UNIQ_TABLE);
    if claims_salvaged {
        storage.purge_all_id_claims()?;
    }
    storage.close()?;
    drop(storage);
    if claims_salvaged {
        // The open takes the `_id` claims again from the documents.
        let storage = Storage::open_with_options(home, opts)?;
        storage.close()?;
    }
    report.rebuilt = rebuilt;
    report.duplicate_ids = duplicates;
    Ok(report)
}

/// Whether a `check_collection` found the indexes and documents apart.
fn check_disagrees(check: &Document) -> bool {
    check.get_i64("undecodable").unwrap_or(0) > 0
        || check
            .get_array("indexes")
            .into_iter()
            .flatten()
            .filter_map(Bson::as_document)
            .any(|ix| {
                ix.get_i64("missing").unwrap_or(0) > 0 || ix.get_i64("extra").unwrap_or(0) > 0
            })
}

/// Step 1: verify every table, salvaging each that fails, on a bare
/// connection.
fn verify_and_salvage(
    home: &str,
    config: &str,
    report: &mut RepairReport,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    let conn = match Connection::open(home, config) {
        Ok(conn) => conn,
        Err(e) if e.is_try_salvage() => {
            log("WiredTiger metadata corruption detected: opening with salvage=true");
            report.metadata_salvaged = true;
            Connection::open(home, &format!("{config},salvage=true"))?
        }
        Err(e) => return Err(e.into()),
    };
    let session = conn.open_session()?;
    let mut tables = Vec::new();
    {
        let cursor = session.open_cursor("metadata:", None)?;
        cursor.set_key_s("table:");
        let mut more = match cursor.search_near() {
            Ok(exact) if exact < 0 => cursor.next()?,
            Ok(_) => true,
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        while more {
            let key = cursor.get_key_s()?;
            if !key.starts_with("table:") {
                break;
            }
            tables.push(key);
            more = cursor.next()?;
        }
    }
    for table in tables {
        match session.verify(&table, None) {
            Ok(()) => {
                log(&format!("Verify succeeded. Not salvaging. {table}"));
                report.verified.push(table);
            }
            Err(e) if e.is_busy() => return Err(e.into()),
            Err(e) => {
                log(&format!(
                    "Verify failed ({e}). Running a salvage operation. {table}"
                ));
                session.salvage(&table, None)?;
                session.verify(&table, None)?;
                report.salvaged.push(table);
            }
        }
    }
    session.checkpoint(None)?;
    drop(session);
    drop(conn);
    Ok(())
}

impl Storage {
    /// Rebuild `db.coll`'s `_id` index and its other indexes from its
    /// documents, as a repair does. Answers how many documents were left out
    /// of the `_id` index for sharing an `_id` with an earlier one (lowest
    /// RecordId first). Not oplogged: nothing a change stream should see.
    pub fn rebuild_indexes(&self, db: &str, coll: &str) -> Result<u64> {
        let _planner = PlannerScope::new();
        note_planner_write();
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let ns_lock = self.coll_lock(db, coll);
        let _c = ns_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _gen = self.ddl_generation_scope();
        let session = self.conn.open_session()?;

        // The `_id` index: emptied, then one row per document.
        loop {
            session.begin_transaction(None)?;
            let removed = (|| -> Result<usize> {
                let cur = session.open_cursor(NAT_SEQ_TABLE, None)?;
                cur.set_key_ssu(db, coll, b"");
                let mut more = match cur.search_near() {
                    Ok(cmp) if cmp < 0 => cur.next()?,
                    Ok(_) => true,
                    Err(e) if e.is_not_found() => false,
                    Err(e) => return Err(e.into()),
                };
                let mut keys = Vec::new();
                while more && keys.len() < PURGE_CHUNK_MAX_ROWS {
                    let (d, c, k) = cur.get_key_ssu()?;
                    if d != db || c != coll {
                        break;
                    }
                    keys.push(k);
                    more = cur.next()?;
                }
                let del = session.open_cursor(NAT_SEQ_TABLE, None)?;
                for k in &keys {
                    del.set_key_ssu(db, coll, k);
                    del.remove()?;
                }
                Ok(keys.len())
            })();
            match removed {
                Ok(n) => {
                    commit_stamped(&session, &self.oplog, self.timestamped)?;
                    if n < PURGE_CHUNK_MAX_ROWS {
                        break;
                    }
                }
                Err(e) => {
                    session.rollback_transaction(None)?;
                    return Err(e);
                }
            }
        }
        let mut rows: Vec<(i64, Vec<u8>)> = Vec::new();
        self.visit_docs(&session, db, coll, |recordid, idk, _| {
            rows.push((recordid, idk.to_vec()));
            Ok(())
        })?;
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut duplicates = 0u64;
        for chunk in rows.chunks(PURGE_CHUNK_MAX_ROWS) {
            session.begin_transaction(None)?;
            let written = (|| -> Result<u64> {
                let cur = session.open_cursor(NAT_SEQ_TABLE, None)?;
                let mut dup = 0u64;
                for (recordid, idk) in chunk {
                    if !seen.insert(idk.clone()) {
                        dup += 1;
                        continue;
                    }
                    cur.set_key_ssu(db, coll, idk);
                    cur.set_value_q(*recordid);
                    cur.insert()?;
                }
                Ok(dup)
            })();
            match written {
                Ok(dup) => {
                    commit_stamped(&session, &self.oplog, self.timestamped)?;
                    duplicates += dup;
                }
                Err(e) => {
                    session.rollback_transaction(None)?;
                    return Err(e);
                }
            }
        }

        // Every other index, as `rebuild_outdated_indexes` rebuilds one:
        // its catalog row, entries and claims dropped, then built again.
        let mut indexes: Vec<(String, Document, Document)> = Vec::new();
        {
            let cur = session.open_cursor(IDX_TABLE, None)?;
            cur.set_key_sss(db, coll, "");
            let mut more = match cur.search_near() {
                Ok(cmp) if cmp < 0 => cur.next()?,
                Ok(_) => true,
                Err(e) if e.is_not_found() => false,
                Err(e) => return Err(e.into()),
            };
            while more {
                let (d, c, name) = cur.get_key_sss()?;
                if d != db || c != coll {
                    break;
                }
                let blob = cur.get_value_u()?;
                if !blob.is_empty() && name != ID_INDEX_NAME {
                    let payload = decode_doc(&blob)?;
                    let key = payload.get_document("key").cloned().unwrap_or_default();
                    let mut options = payload.get_document("options").cloned().unwrap_or_default();
                    for stamp in BUILD_STAMPS {
                        options.remove(*stamp);
                    }
                    indexes.push((name, key, options));
                }
                more = cur.next()?;
            }
        }
        for (name, key, options) in &indexes {
            loop {
                session.begin_transaction(None)?;
                let removed = self.delete_entries_prefix_upto(
                    &session,
                    db,
                    coll,
                    name,
                    Some(PURGE_CHUNK_MAX_ROWS),
                );
                match removed {
                    Ok(n) => {
                        commit_stamped(&session, &self.oplog, self.timestamped)?;
                        if n < PURGE_CHUNK_MAX_ROWS {
                            break;
                        }
                    }
                    Err(e) => {
                        session.rollback_transaction(None)?;
                        return Err(e);
                    }
                }
            }
            session.begin_transaction(None)?;
            let outcome = (|| -> Result<()> {
                let c = session.open_cursor(IDX_TABLE, None)?;
                c.set_key_sss(db, coll, name);
                c.search()?;
                c.remove()?;
                self.purge_unique_claims(&session, db, coll, Some(name))?;
                self.create_index_in_txn(&session, db, coll, name, key, options, false)?;
                Ok(())
            })();
            match outcome {
                Ok(()) => commit_stamped(&session, &self.oplog, self.timestamped)?,
                Err(e) => {
                    session.rollback_transaction(None)?;
                    return Err(e);
                }
            }
        }
        Ok(duplicates)
    }

    /// Drop every `_id` claim and the markers that say they were taken, so
    /// the next open takes them again from the documents (a repair whose
    /// claims table was salvaged).
    pub(crate) fn purge_all_id_claims(&self) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let session = self.conn.open_session()?;
        session.begin_transaction(None)?;
        match self.purge_id_claims(&session) {
            Ok(()) => commit_stamped(&session, &self.oplog, self.timestamped),
            Err(e) => {
                session.rollback_transaction(None)?;
                Err(e)
            }
        }
    }
}

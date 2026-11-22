//! The size storer: each collection's record count, data size and index
//! sizes, kept current by every write instead of counted when asked
//! (RM-191).
//!
//! This is mongod's mechanism. Its `WiredTigerRecordStore` keeps
//! `numRecords` and `dataSize` in memory and moves them at write time, in the
//! write's own storage transaction, with a rollback handler that moves them
//! back if that transaction aborts (`RecoveryUnit::onRollback`). So a fast
//! count sees another transaction's uncommitted insert, and stops seeing it
//! when the transaction aborts (measured on mongod 8.2.11: a `count` with no
//! query answered 3 for 2 committed documents and one inserted by an open
//! transaction, and 2 after the abort). The numbers are written to its
//! `sizeStorer` table from time to time and at shutdown, and read back at
//! startup; `validate` corrects them when a walk of the collection disagrees.
//!
//! Here the counts move where a document row or an index entry is written:
//! [`SizeStorer::adjust`] applies a change at once and, inside a WiredTiger
//! transaction, registers its undo with the session
//! ([`secantus_wt::Session::on_transaction_end`]), which runs it if the
//! transaction rolls back. Every row change counts the same way, whatever
//! made it -- an insert, a capped collection's eviction, a drop's purge, a
//! rename's copy, crash recovery's replay -- so the counts follow the rows
//! and no path has to know about another.
//!
//! What is kept, per collection, is what `collStats` reports: the documents,
//! their BSON bytes, the `_id` index's bytes (its keys) and each other
//! index's bytes (its entries), in the units the walk they replace measured
//! (`Storage::collection_data_size`, `Storage::index_sizes`).
//!
//! Persistence is mongod's `sizeStorer` (phase 6 of
//! `tasks/mongod-storage-model-plan.md`, measured on mongod 8.2.11): a table
//! [`SIZE_STORER_TABLE`] (`key_format=u,value_format=u`, otherwise
//! WiredTiger's defaults, logged), keyed by the ident's table URI as raw
//! bytes (`table:collection-<UUID>`, `table:_mdb_catalog`, no terminator),
//! the value the 43-byte BSON `{numRecords: Int64, dataSize: Int64}`. It has
//! a row for every record store whose sizes ever moved -- collections, the
//! oplog and `_mdb_catalog` itself; one created and never written has none,
//! one emptied keeps `0, 0` -- and none for an index. A rename keeps the
//! ident, so it keeps the row; a drop takes the row away with the ident.
//!
//! What mongod's size storer does not hold lives in this server's own
//! [`PRIVATE_TABLE`], keyed the same way: each index's entry bytes, by the
//! index's ident (mongod reports an index's size from its file instead,
//! RM-040), and a clean-close marker. Both are written at a clean close, in
//! one transaction. Opening a store takes the marker away before any write,
//! so a store that did not close cleanly has none, and its counts are made
//! again by one walk of its tables. That is the answer a mongod replica-set
//! member reaches for a collection after a crash (its recovery replays the
//! oplog from the last checkpoint and adjusts the counts as it goes; measured
//! 8.2.11: 1,001 documents written, the process killed before the size
//! storer was flushed, 1,001 counted after the restart), and this server
//! presents itself as one. (mongod flushes the table at its checkpoints as
//! well; this server only at a clean close, which is the only state the
//! next open trusts.)

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bson::Bson;
use secantus_wt::Session;

/// mongod's size storer: ident URI -> `{numRecords, dataSize}`.
pub(crate) const SIZE_STORER_TABLE: &str = "table:sizeStorer";
/// The configuration mongod 8.2.11 creates it with: raw keys and values,
/// WiredTiger's defaults for the rest (no compressor, 4KB internal and 32KB
/// leaf pages, no prefix compression, logged).
pub(crate) const SIZE_STORER_CFG: &str = "key_format=u,value_format=u";

/// This server's own sizes, beside mongod's: index ident URI -> `{size}`,
/// and the clean-close marker.
pub(crate) const PRIVATE_TABLE: &str = "table:secantus_size_storer";
pub(crate) const PRIVATE_CFG: &str = "key_format=u,value_format=u";

/// The marker row's key in [`PRIVATE_TABLE`]: no ident URI has this form.
const MARKER_KEY: &[u8] = b"secantus:cleanClose";

/// The ident URI of `_mdb_catalog`, as its size-storer row is keyed.
const CATALOG_KEY: &str = crate::mdb_catalog::CATALOG_TABLE;

/// A size-storer value as mongod 8.2.11 writes it: `{numRecords: Int64,
/// dataSize: Int64}`, 43 bytes.
pub fn size_storer_value(records: i64, data_size: i64) -> Vec<u8> {
    let d = bson::doc! {"numRecords": records, "dataSize": data_size};
    secantus_core::bsondoc::to_vec(&d).expect("a fixed document always encodes")
}

/// `(numRecords, dataSize)` from a size-storer value; `None` for one that is
/// not in that form.
fn parse_size_storer_value(bytes: &[u8]) -> Option<(i64, i64)> {
    let d = crate::decode_doc(bytes).ok()?;
    Some((num(d.get("numRecords")?)?, num(d.get("dataSize")?)?))
}

fn num(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int64(n) => Some(*n),
        Bson::Int32(n) => Some(i64::from(*n)),
        _ => None,
    }
}

/// One collection's sizes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionSizes {
    /// How many documents it holds (mongod's `numRecords`).
    pub records: i64,
    /// Their BSON bytes (mongod's `dataSize`).
    pub data_size: i64,
    /// The `_id` index's bytes: the summed `_id` keys.
    pub id_index_size: i64,
    /// Every other index's bytes, by name: its summed entries. An index
    /// with none is absent.
    pub index_sizes: BTreeMap<String, i64>,
}

impl CollectionSizes {
    fn apply(&mut self, d: &Delta, sign: i64) {
        self.records += sign * d.records;
        self.data_size += sign * d.data_size;
        self.id_index_size += sign * d.id_index_size;
        for (name, bytes) in &d.entries {
            if name == crate::ID_INDEX_NAME {
                self.id_index_size += sign * bytes;
                continue;
            }
            let slot = self.index_sizes.entry(name.clone()).or_insert(0);
            *slot += sign * bytes;
            if *slot == 0 {
                self.index_sizes.remove(name);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.records == 0
            && self.data_size == 0
            && self.id_index_size == 0
            && self.index_sizes.is_empty()
    }

    fn minus(&self, other: &CollectionSizes) -> CollectionSizes {
        let mut out = self.clone();
        out.apply(&Delta::from_sizes(other), -1);
        out
    }
}

/// A change to one collection's sizes, made by one row write.
#[derive(Clone, Debug, Default)]
pub(crate) struct Delta {
    records: i64,
    data_size: i64,
    id_index_size: i64,
    entries: Vec<(String, i64)>,
}

impl Delta {
    /// A document row of `blob` bytes was written where none was.
    pub(crate) fn insert(blob: usize) -> Delta {
        Delta {
            records: 1,
            data_size: blob as i64,
            ..Delta::default()
        }
    }

    /// A document row of `blob` bytes was removed.
    pub(crate) fn remove(blob: usize) -> Delta {
        Delta {
            records: -1,
            data_size: -(blob as i64),
            ..Delta::default()
        }
    }

    /// Everything `sizes` counts went with the tables that held it: the
    /// documents' table and each index's.
    pub(crate) fn drop_all(sizes: &CollectionSizes) -> Delta {
        let mut d = Delta::from_sizes(sizes);
        d.records = -d.records;
        d.data_size = -d.data_size;
        d.id_index_size = -d.id_index_size;
        for (_, bytes) in &mut d.entries {
            *bytes = -*bytes;
        }
        d
    }

    /// Everything `sizes` counts arrived with the tables that hold it, as a
    /// rename moves them.
    pub(crate) fn arrive(sizes: &CollectionSizes) -> Delta {
        Delta::from_sizes(sizes)
    }

    /// A document row was rewritten in place.
    pub(crate) fn replace(old: usize, new: usize) -> Delta {
        Delta {
            data_size: new as i64 - old as i64,
            ..Delta::default()
        }
    }

    /// Index `name` gained (`bytes > 0`) or lost entries of that many bytes.
    /// The `_id` index's are counted as `idIndexSize`.
    pub(crate) fn entries(name: &str, bytes: i64) -> Delta {
        Delta {
            entries: vec![(name.to_string(), bytes)],
            ..Delta::default()
        }
    }

    fn from_sizes(s: &CollectionSizes) -> Delta {
        Delta {
            records: s.records,
            data_size: s.data_size,
            id_index_size: s.id_index_size,
            entries: s.index_sizes.iter().map(|(n, b)| (n.clone(), *b)).collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.records == 0
            && self.data_size == 0
            && self.id_index_size == 0
            && self.entries.iter().all(|(_, b)| *b == 0)
    }
}

/// One collection's counts: `live` is what readers see, uncommitted
/// changes included (mongod's numbers); `pending` is the part of it that
/// transactions still running made, so `live - pending` is what has
/// committed.
#[derive(Default)]
struct Slot {
    live: CollectionSizes,
    pending: CollectionSizes,
}

type Slots = HashMap<String, HashMap<String, Arc<Mutex<Slot>>>>;

/// Every collection's sizes (see the module docs).
#[derive(Default)]
pub(crate) struct SizeStorer {
    slots: Mutex<Slots>,
    /// How many times the counts were made by walking the store.
    recounts: AtomicU64,
    /// The oplog's `(entries, bytes)` the last clean close left, read by
    /// [`Self::load`] when the counts are trusted: mongod's size storer keeps
    /// the oplog's record count and data size beside every collection's.
    oplog: Mutex<Option<(i64, i64)>>,
}

impl SizeStorer {
    fn slot(&self, db: &str, coll: &str) -> Arc<Mutex<Slot>> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get(db).and_then(|m| m.get(coll)) {
            return slot.clone();
        }
        slots
            .entry(db.to_string())
            .or_default()
            .entry(coll.to_string())
            .or_default()
            .clone()
    }

    /// Apply `delta` to `(db, coll)` now; if `session` is in a transaction,
    /// take it back should that transaction roll back.
    pub(crate) fn adjust(&self, session: &Session, db: &str, coll: &str, delta: Delta) {
        if delta.is_empty() {
            return;
        }
        let slot = self.slot(db, coll);
        let in_txn = session.in_transaction();
        {
            let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
            s.live.apply(&delta, 1);
            if in_txn {
                s.pending.apply(&delta, 1);
            }
        }
        if in_txn {
            session.on_transaction_end(Box::new(move |committed| {
                let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
                s.pending.apply(&delta, -1);
                if !committed {
                    s.live.apply(&delta, -1);
                }
            }));
        }
    }

    /// `(db, coll)`'s sizes as readers see them: uncommitted changes
    /// included, as mongod's are.
    pub(crate) fn get(&self, db: &str, coll: &str) -> CollectionSizes {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .get(db)
            .and_then(|m| m.get(coll))
            .map(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).live.clone())
            .unwrap_or_default()
    }

    /// Make `(db, coll)`'s committed sizes `walked` -- what a walk of the
    /// collection found -- keeping the changes transactions still running
    /// have made on top. What `validate` does with what it counted.
    pub(crate) fn correct(&self, db: &str, coll: &str, walked: CollectionSizes) {
        let slot = self.slot(db, coll);
        let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
        let mut live = walked;
        live.apply(&Delta::from_sizes(&s.pending), 1);
        s.live = live;
    }

    /// `(db, coll)`'s committed sizes.
    pub(crate) fn committed_of(&self, db: &str, coll: &str) -> CollectionSizes {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .get(db)
            .and_then(|m| m.get(coll))
            .map(|slot| {
                let s = slot.lock().unwrap_or_else(|e| e.into_inner());
                s.live.minus(&s.pending)
            })
            .unwrap_or_default()
    }

    /// Every collection's committed sizes, the empty ones left out.
    pub(crate) fn committed_sizes(&self) -> Vec<(String, String, CollectionSizes)> {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        for (db, colls) in slots.iter() {
            for (coll, slot) in colls {
                let s = slot.lock().unwrap_or_else(|e| e.into_inner());
                let committed = s.live.minus(&s.pending);
                if !committed.is_empty() {
                    out.push((db.clone(), coll.clone(), committed));
                }
            }
        }
        out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        out
    }

    /// Start from `sizes` (what was loaded or walked at open).
    fn replace_all(&self, sizes: HashMap<(String, String), CollectionSizes>) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.clear();
        for ((db, coll), live) in sizes {
            slots.entry(db).or_default().insert(
                coll,
                Arc::new(Mutex::new(Slot {
                    live,
                    pending: CollectionSizes::default(),
                })),
            );
        }
    }

    /// How many times the counts were made by walking the store.
    pub(crate) fn recounts(&self) -> u64 {
        self.recounts.load(Ordering::Relaxed)
    }

    /// Whether `(db, coll)` has a slot: its sizes moved since the open, or
    /// were loaded. mongod writes a size-storer row for such a record store
    /// even when its counts are back to zero.
    fn has_slot(&self, db: &str, coll: &str) -> bool {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.get(db).is_some_and(|m| m.contains_key(coll))
    }

    /// The oplog state this server's last clean close recorded with its
    /// marker, read without taking it away: `None` when the store has no
    /// marker (a crash, a store this server never closed, or no
    /// [`PRIVATE_TABLE`] yet). A marker whose state is not the store's now
    /// means another server (mongod) wrote the store after that close.
    pub(crate) fn clean_close_marker(session: &Session) -> crate::Result<Option<Vec<i64>>> {
        let private = match session.open_cursor(PRIVATE_TABLE, None) {
            Ok(c) => c,
            Err(e) if e.is_not_found() || e.is_missing_table() => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        private.set_key_u(MARKER_KEY);
        match private.search() {
            Ok(()) => {
                let marker = crate::decode_doc(&private.get_value_u()?)?;
                Ok(Some(
                    marker
                        .get_array("oplog")
                        .map(|a| a.iter().filter_map(Bson::as_i64).collect())
                        .unwrap_or_default(),
                ))
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load the counts a clean close left, taking its marker away in the
    /// same transaction so that a crash from here on is noticed at the next
    /// open. `fingerprint` is the oplog state this open recovered: a store
    /// some other build wrote since (one that kept no counts) has moved it,
    /// and its counts are not trusted. `placements` maps each collection to
    /// the ident its row is keyed by, and `oplog_uri` names the oplog's.
    /// `false` when there is nothing to trust and the caller must count.
    pub(crate) fn load(
        &self,
        session: &Session,
        fingerprint: &[i64],
        placements: &[(String, String, crate::mdb_catalog::Placement)],
        oplog_uri: &str,
    ) -> crate::Result<bool> {
        let private = session.open_cursor(PRIVATE_TABLE, None)?;
        private.set_key_u(MARKER_KEY);
        let trusted = match private.search() {
            Ok(()) => {
                let marker = crate::decode_doc(&private.get_value_u()?)?;
                let left: Vec<i64> = marker
                    .get_array("oplog")
                    .map(|a| a.iter().filter_map(Bson::as_i64).collect())
                    .unwrap_or_default();
                left == fingerprint
            }
            Err(e) if e.is_not_found() => false,
            Err(e) => return Err(e.into()),
        };
        if !trusted {
            return Ok(false);
        }
        let mongods = session.open_cursor(SIZE_STORER_TABLE, None)?;
        // `table`'s row, if it has one.
        let row = |cur: &secantus_wt::Cursor, table: &str| -> crate::Result<Option<Vec<u8>>> {
            cur.reset()?;
            cur.set_key_u(table.as_bytes());
            match cur.search() {
                Ok(()) => Ok(Some(cur.get_value_u()?)),
                Err(e) if e.is_not_found() => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let mut sizes = HashMap::new();
        for (db, coll, placement) in placements {
            // A collection with no row never had a size move: it is empty.
            let mut s = CollectionSizes::default();
            let has_row = match row(&mongods, &placement.table())? {
                Some(v) => {
                    let Some((records, data_size)) = parse_size_storer_value(&v) else {
                        return Ok(false);
                    };
                    s.records = records;
                    s.data_size = data_size;
                    true
                }
                None => false,
            };
            for (name, ident) in &placement.indexes {
                // A clean close writes every index's row, zero or not.
                let Some(v) = row(&private, &crate::mdb_catalog::table_uri(ident))? else {
                    return Ok(false);
                };
                let Some(bytes) = crate::decode_doc(&v)
                    .ok()
                    .and_then(|d| d.get("size").and_then(num))
                else {
                    return Ok(false);
                };
                if name == crate::ID_INDEX_NAME {
                    s.id_index_size = bytes;
                } else if bytes != 0 {
                    s.index_sizes.insert(name.clone(), bytes);
                }
            }
            if has_row || !s.is_empty() {
                sizes.insert((db.clone(), coll.clone()), s);
            }
        }
        let oplog_sizes = match row(&mongods, oplog_uri)? {
            Some(v) => match parse_size_storer_value(&v) {
                Some(s) => s,
                None => return Ok(false),
            },
            None => (0, 0),
        };
        drop(mongods);
        private.reset()?;
        drop(private);
        // The marker goes in its own committed transaction, before the store
        // takes any write: every later commit is behind it in the log and in
        // any checkpoint, so no crash can keep the marker and lose it.
        session.begin_transaction(None)?;
        let del = session.open_cursor(PRIVATE_TABLE, None)?;
        del.set_key_u(MARKER_KEY);
        if let Err(e) = del.remove() {
            rollback_after(session, "taking the clean-close marker away");
            return Err(e.into());
        }
        drop(del);
        session.commit_transaction(None)?;
        self.replace_all(sizes);
        *self.oplog.lock().unwrap_or_else(|e| e.into_inner()) = Some(oplog_sizes);
        Ok(true)
    }

    /// Make every collection's counts by walking the document and
    /// index-entry tables once: for a store with no clean close to trust.
    /// Runs at open, before the store takes any write.
    pub(crate) fn recount(
        &self,
        session: &Session,
        placements: &[(String, String, crate::mdb_catalog::Placement)],
    ) -> crate::Result<()> {
        let mut sizes: HashMap<(String, String), CollectionSizes> = HashMap::new();
        for (db, coll, placement) in placements {
            // The collection's own table (`collection-<UUID>`), keyed by RecordId.
            let cur = match session.open_cursor(&placement.table(), None) {
                Ok(c) => Some(c),
                Err(e) if e.is_missing_table() => None,
                Err(e) => return Err(e.into()),
            };
            if let Some(cur) = cur {
                while cur.next()? {
                    let value = cur.get_value_u()?;
                    let blob = value.as_slice();
                    let s = sizes.entry((db.clone(), coll.clone())).or_default();
                    s.apply(&Delta::insert(blob.len()), 1);
                }
            }
            // Each secondary index's own table (`index-<UUID>`).
            for (name, ident) in &placement.indexes {
                let cur = match session.open_cursor(&crate::mdb_catalog::table_uri(ident), None) {
                    Ok(c) => c,
                    Err(e) if e.is_missing_table() => continue,
                    Err(e) => return Err(e.into()),
                };
                while cur.next()? {
                    let len = cur.with_key_u(<[u8]>::len)?;
                    let s = sizes.entry((db.clone(), coll.clone())).or_default();
                    s.apply(&Delta::entries(name, len as i64), 1);
                }
            }
        }
        self.replace_all(sizes);
        self.recounts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// The oplog's `(entries, bytes)` that [`Self::load`] trusted, once.
    pub(crate) fn take_oplog_sizes(&self) -> Option<(i64, i64)> {
        self.oplog.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Write the committed counts to mongod's size storer, each index's
    /// bytes and the clean-close marker to [`PRIVATE_TABLE`], in one
    /// transaction. Runs at a clean close, when nothing else writes.
    ///
    /// A row goes in for every ident whose sizes moved (a slot), every one
    /// that has a row already, and the oplog and `_mdb_catalog`; rows of
    /// idents the catalog no longer names go. Sizes kept for a namespace or
    /// an index with no ident to key them by cannot be written: the marker
    /// is then left out, so the next open counts again rather than trust
    /// counts that lost something.
    pub(crate) fn persist(
        &self,
        session: &Session,
        fingerprint: &[i64],
        placements: &[(String, String, crate::mdb_catalog::Placement)],
        oplog: (&str, i64, i64),
    ) -> crate::Result<()> {
        let catalog = catalog_sizes(session)?;
        let mut unkeyed = Vec::new();
        {
            let placed: HashMap<(&str, &str), &crate::mdb_catalog::Placement> = placements
                .iter()
                .map(|(db, coll, p)| ((db.as_str(), coll.as_str()), p))
                .collect();
            for (db, coll, sizes) in self.committed_sizes() {
                match placed.get(&(db.as_str(), coll.as_str())) {
                    None => unkeyed.push(format!("{db}.{coll}")),
                    Some(p) => {
                        if sizes.id_index_size != 0 && !p.indexes.contains_key(crate::ID_INDEX_NAME)
                        {
                            unkeyed.push(format!("{db}.{coll} index {}", crate::ID_INDEX_NAME));
                        }
                        for name in sizes.index_sizes.keys() {
                            if !p.indexes.contains_key(name) {
                                unkeyed.push(format!("{db}.{coll} index {name}"));
                            }
                        }
                    }
                }
            }
        }
        if !unkeyed.is_empty() {
            eprintln!(
                "secantus-storage: sizes kept for {} have no ident to be saved under; \
                 the next open counts the store again",
                unkeyed.join(", ")
            );
        }
        session.begin_transaction(None)?;
        let written = (|| -> crate::Result<()> {
            let mongods = session.open_cursor(SIZE_STORER_TABLE, None)?;
            let had: std::collections::HashSet<Vec<u8>> = {
                let mut had = std::collections::HashSet::new();
                while mongods.next()? {
                    had.insert(mongods.get_key_u()?);
                }
                had
            };
            let mut rows: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
            let mut private_rows: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
            for (db, coll, p) in placements {
                let key = p.table().into_bytes();
                let committed = self.committed_of(db, coll);
                if self.has_slot(db, coll) || had.contains(&key) || !committed.is_empty() {
                    rows.insert(
                        key,
                        size_storer_value(committed.records, committed.data_size),
                    );
                }
                for (name, ident) in &p.indexes {
                    let bytes = if name == crate::ID_INDEX_NAME {
                        committed.id_index_size
                    } else {
                        committed.index_sizes.get(name).copied().unwrap_or(0)
                    };
                    private_rows.insert(
                        crate::mdb_catalog::table_uri(ident).into_bytes(),
                        crate::encode_doc(&bson::doc! {"size": bytes})?,
                    );
                }
            }
            let (oplog_uri, oplog_records, oplog_bytes) = oplog;
            rows.insert(
                oplog_uri.as_bytes().to_vec(),
                size_storer_value(oplog_records, oplog_bytes),
            );
            rows.insert(
                CATALOG_KEY.as_bytes().to_vec(),
                size_storer_value(catalog.0, catalog.1),
            );
            for key in had.iter().filter(|k| !rows.contains_key(*k)) {
                mongods.reset()?;
                mongods.set_key_u(key);
                mongods.remove()?;
            }
            for (key, value) in &rows {
                mongods.reset()?;
                mongods.set_key_u(key);
                mongods.set_value_u(value);
                mongods.insert()?;
            }
            drop(mongods);
            let private = session.open_cursor(PRIVATE_TABLE, None)?;
            let mut stale = Vec::new();
            while private.next()? {
                stale.push(private.get_key_u()?);
            }
            for key in stale {
                private.reset()?;
                private.set_key_u(&key);
                private.remove()?;
            }
            for (key, value) in &private_rows {
                private.reset()?;
                private.set_key_u(key);
                private.set_value_u(value);
                private.insert()?;
            }
            if unkeyed.is_empty() {
                let marker = bson::doc! {
                    "clean": true,
                    "oplog": fingerprint.iter().map(|n| Bson::Int64(*n)).collect::<Vec<_>>(),
                };
                private.reset()?;
                private.set_key_u(MARKER_KEY);
                private.set_value_u(&crate::encode_doc(&marker)?);
                private.insert()?;
            }
            Ok(())
        })();
        match written {
            Ok(()) => session.commit_transaction(None).map_err(Into::into),
            Err(e) => {
                rollback_after(session, "saving the collection sizes");
                Err(e)
            }
        }
    }
}

/// `_mdb_catalog`'s own `(records, bytes)`, for its size-storer row: mongod
/// keeps the catalog's counts like any record store's. Read at a clean close;
/// the catalog holds a row per collection.
fn catalog_sizes(session: &Session) -> crate::Result<(i64, i64)> {
    let cur = session.open_cursor(CATALOG_KEY, None)?;
    let (mut records, mut bytes) = (0i64, 0i64);
    while cur.next()? {
        records += 1;
        bytes += cur.with_value_u(|v| v.len() as i64)?;
    }
    Ok((records, bytes))
}

/// Roll back a transaction a failed `what` leaves open. The failure is what
/// the caller answers; a rollback that fails as well is logged, never
/// dropped.
fn rollback_after(session: &Session, what: &str) {
    if let Err(e) = session.rollback_transaction(None) {
        eprintln!("secantus-storage: rolling back after {what} failed too: {e:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_and_its_negation_cancel() {
        let mut s = CollectionSizes::default();
        let doc = Delta::insert(40);
        let entry = Delta::entries("a_1", 12);
        s.apply(&doc, 1);
        s.apply(&entry, 1);
        assert_eq!(s.records, 1);
        assert_eq!(s.index_sizes.get("a_1"), Some(&12));
        s.apply(&entry, -1);
        s.apply(&doc, -1);
        assert!(s.is_empty());
    }

    #[test]
    fn a_size_storer_value_is_mongods() {
        // mongod 8.2.11's row for a collection of 3 documents, 69 bytes
        // (measured, phase 6): `{numRecords: NumberLong(3), dataSize:
        // NumberLong(69)}`.
        let measured = "2b000000126e756d5265636f726473000300000000000000\
                        126461746153697a6500450000000000000000";
        let hex: String = size_storer_value(3, 69)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, measured);
        assert_eq!(
            parse_size_storer_value(&size_storer_value(3, 69)),
            Some((3, 69))
        );
    }
}

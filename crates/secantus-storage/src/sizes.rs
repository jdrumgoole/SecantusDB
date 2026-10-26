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
//! Persistence: [`SIZE_STORER_TABLE`] holds a row per collection, written at
//! a clean close with the committed counts, beside a marker saying the close
//! was clean. Opening a store takes the marker away before any write, so a
//! store that did not close cleanly has none, and its counts are made again
//! by one walk of its tables. That is the answer a mongod replica-set member
//! reaches after a crash (its recovery replays the oplog from the last
//! checkpoint and adjusts the counts as it goes; measured 8.2.11: 1,300
//! documents written, the process killed, 1,300 counted after the restart),
//! and this server presents itself as one. A store written before the size
//! storer existed has no marker either.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bson::{Bson, Document};
use secantus_wt::Session;

/// The table the counts are kept in at rest: `(db, coll) -> BSON`.
pub(crate) const SIZE_STORER_TABLE: &str = "table:secantus_size_storer";
pub(crate) const SIZE_STORER_CFG: &str = "key_format=SS,value_format=u";

/// The marker row's key: no collection has an empty database name.
const MARKER: (&str, &str) = ("", "");

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

    fn to_bson(&self) -> Document {
        let mut indexes = Document::new();
        for (name, bytes) in &self.index_sizes {
            indexes.insert(name.clone(), Bson::Int64(*bytes));
        }
        bson::doc! {
            "numRecords": self.records,
            "dataSize": self.data_size,
            "idIndexSize": self.id_index_size,
            "indexSizes": indexes,
        }
    }

    fn from_bson(d: &Document) -> Option<CollectionSizes> {
        let num = |v: &Bson| match v {
            Bson::Int64(n) => Some(*n),
            Bson::Int32(n) => Some(i64::from(*n)),
            _ => None,
        };
        let mut out = CollectionSizes {
            records: num(d.get("numRecords")?)?,
            data_size: num(d.get("dataSize")?)?,
            id_index_size: num(d.get("idIndexSize")?)?,
            index_sizes: BTreeMap::new(),
        };
        for (name, v) in d.get_document("indexSizes").ok()? {
            out.index_sizes.insert(name.clone(), num(v)?);
        }
        Some(out)
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
    /// A document row of `id_key` and `blob` bytes was written where none was.
    pub(crate) fn insert(id_key: usize, blob: usize) -> Delta {
        Delta {
            records: 1,
            data_size: blob as i64,
            id_index_size: id_key as i64,
            entries: Vec::new(),
        }
    }

    /// A document row of `id_key` and `blob` bytes was removed.
    pub(crate) fn remove(id_key: usize, blob: usize) -> Delta {
        Delta {
            records: -1,
            data_size: -(blob as i64),
            id_index_size: -(id_key as i64),
            entries: Vec::new(),
        }
    }

    /// A document row was rewritten in place.
    pub(crate) fn replace(old: (usize, usize), new: (usize, usize)) -> Delta {
        Delta {
            records: 0,
            data_size: new.1 as i64 - old.1 as i64,
            id_index_size: new.0 as i64 - old.0 as i64,
            entries: Vec::new(),
        }
    }

    /// Index `name` gained (`bytes > 0`) or lost entries of that many bytes.
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

    /// Load the counts a clean close left, taking its marker away in the
    /// same transaction so that a crash from here on is noticed at the next
    /// open. `fingerprint` is the oplog state this open recovered: a store
    /// some other build wrote since (one that kept no counts) has moved it,
    /// and its counts are not trusted. `false` when there is nothing to
    /// trust and the caller must count.
    pub(crate) fn load(&self, session: &Session, fingerprint: &[i64]) -> crate::Result<bool> {
        let cur = session.open_cursor(SIZE_STORER_TABLE, None)?;
        cur.set_key_ss(MARKER.0, MARKER.1);
        let trusted = match cur.search() {
            Ok(()) => {
                let marker = crate::decode_doc(&cur.get_value_u()?)?;
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
        let mut sizes = HashMap::new();
        cur.reset()?;
        while cur.next()? {
            let (db, coll) = cur.get_key_ss()?;
            if (db.as_str(), coll.as_str()) == MARKER {
                continue;
            }
            let row = crate::decode_doc(&cur.get_value_u()?)?;
            let Some(s) = CollectionSizes::from_bson(&row) else {
                return Ok(false);
            };
            sizes.insert((db, coll), s);
        }
        drop(cur);
        // The marker goes in its own committed transaction, before the store
        // takes any write: every later commit is behind it in the log and in
        // any checkpoint, so no crash can keep the marker and lose it.
        session.begin_transaction(None)?;
        let del = session.open_cursor(SIZE_STORER_TABLE, None)?;
        del.set_key_ss(MARKER.0, MARKER.1);
        if let Err(e) = del.remove() {
            rollback_after(session, "taking the clean-close marker away");
            return Err(e.into());
        }
        drop(del);
        session.commit_transaction(None)?;
        self.replace_all(sizes);
        Ok(true)
    }

    /// Make every collection's counts by walking the document and
    /// index-entry tables once: for a store with no clean close to trust.
    /// Runs at open, before the store takes any write.
    pub(crate) fn recount(
        &self,
        session: &Session,
        doc_tables: &[String],
        entries_table: &str,
    ) -> crate::Result<()> {
        let mut sizes: HashMap<(String, String), CollectionSizes> = HashMap::new();
        for table in doc_tables {
            let cur = match session.open_cursor(table, None) {
                Ok(c) => c,
                Err(e) if e.is_missing_table() => continue,
                Err(e) => return Err(e.into()),
            };
            while cur.next()? {
                let (db, coll, _recordid) = cur.get_key_ssq()?;
                let value = cur.get_value_u()?;
                let (idk, blob) = crate::unframe_doc_value(&value)?;
                let s = sizes.entry((db, coll)).or_default();
                s.apply(&Delta::insert(idk.len(), blob.len()), 1);
            }
        }
        let cur = match session.open_cursor(entries_table, None) {
            Ok(c) => Some(c),
            Err(e) if e.is_missing_table() => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(cur) = cur {
            while cur.next()? {
                let (db, coll, name, packed) = cur.get_key_sssu()?;
                let s = sizes.entry((db, coll)).or_default();
                s.apply(&Delta::entries(&name, packed.len() as i64), 1);
            }
        }
        self.replace_all(sizes);
        self.recounts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Write the committed counts and the clean-close marker, replacing what
    /// the table held, in one transaction.
    pub(crate) fn persist(&self, session: &Session, fingerprint: &[i64]) -> crate::Result<()> {
        let committed = self.committed_sizes();
        session.begin_transaction(None)?;
        let written = (|| -> crate::Result<()> {
            let cur = session.open_cursor(SIZE_STORER_TABLE, None)?;
            let mut stale = Vec::new();
            while cur.next()? {
                stale.push(cur.get_key_ss()?);
            }
            for (db, coll) in stale {
                cur.reset()?;
                cur.set_key_ss(&db, &coll);
                cur.remove()?;
            }
            for (db, coll, sizes) in &committed {
                cur.reset()?;
                cur.set_key_ss(db, coll);
                cur.set_value_u(&crate::encode_doc(&sizes.to_bson())?);
                cur.insert()?;
            }
            let marker = bson::doc! {
                "clean": true,
                "oplog": fingerprint.iter().map(|n| Bson::Int64(*n)).collect::<Vec<_>>(),
            };
            cur.reset()?;
            cur.set_key_ss(MARKER.0, MARKER.1);
            cur.set_value_u(&crate::encode_doc(&marker)?);
            cur.insert()?;
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
        let doc = Delta::insert(3, 40);
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
    fn sizes_round_trip_through_bson() {
        let mut s = CollectionSizes {
            records: 7,
            data_size: 700,
            id_index_size: 21,
            index_sizes: BTreeMap::new(),
        };
        s.index_sizes.insert("a_1".into(), 99);
        assert_eq!(CollectionSizes::from_bson(&s.to_bson()), Some(s));
    }
}

//! When each collection's catalog entry last changed, for mongod's rule that
//! a transaction may not write to a collection whose catalog entry changed
//! after its snapshot was taken.
//!
//! mongod keeps a minimum valid snapshot on each collection, moved on by
//! every change to its catalog entry -- an index committed or dropped, a
//! `collMod`, a drop, a rename -- and a transaction whose snapshot predates
//! it fails its write there with 112 WriteConflict "Unable to write to
//! collection '<ns>' due to catalog changes; please retry the operation"
//! (measured 8.2.11, 2026-11-08). Without the rule the write used the index
//! list of its own snapshot, and an index committed since got no entry for
//! the document (RM-320).
//!
//! The server keeps no commit timestamps on its user transactions, so the
//! order is taken from a counter instead: a change bumps
//! [`CATALOG_CHANGES_DONE`] once it is visible, and a transaction notes the
//! counter just before its snapshot is taken. A change it cannot see
//! finished after that, so its mark is above the transaction's. A change
//! that became visible just before the snapshot but bumped the counter just
//! after it is the one false conflict -- transient, and labelled so.
//!
//! The marks are kept per store (a `Storage`'s address, as the shared-lock
//! table keys its rows): one process can hold several stores, each with its
//! own catalog, and a change in one says nothing about another's
//! transactions. A store's marks go when it closes.
//!
//! Which changes count is the MongoDB server's business: it brackets each of
//! them with [`CatalogChange`]. The PostgreSQL server, whose catalog reads
//! follow PostgreSQL's rules, never does, so nothing here touches it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

/// Catalog changes that have become visible.
static CATALOG_CHANGES_DONE: AtomicU64 = AtomicU64::new(0);
/// Catalog changes being made now.
static CATALOG_CHANGES_INFLIGHT: AtomicU64 = AtomicU64::new(0);

/// A store, and a `(db, collection)` in it -- the collection `""` standing
/// for every collection of the database.
type Key = (usize, String, String);

/// Per [`Key`], the changes in flight, and the counter's value after the
/// last one finished.
static MARKS: LazyLock<Mutex<HashMap<Key, (u32, u64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The counter, read just before a transaction's snapshot is taken.
pub(crate) fn catalog_epoch() -> u64 {
    CATALOG_CHANGES_DONE.load(Ordering::SeqCst)
}

/// A change to catalog entries, from before it is made until it is visible
/// to every session. While it is alive a transaction writing one of its
/// collections conflicts; [`Self::committed`] marks the change as visible,
/// and dropping it without that (the change failed) leaves the marks as
/// they were.
pub struct CatalogChange {
    keys: Vec<Key>,
    committed: bool,
    /// Counted in [`CATALOG_CHANGES_INFLIGHT`] (not made inside a user
    /// transaction).
    counted: bool,
}

impl CatalogChange {
    /// About to change the catalog entries of `colls` in `store`, each `(db,
    /// collection)`; a collection of `""` is every collection of `db`.
    ///
    /// Inside a user transaction this marks nothing: the only catalog change
    /// mongod lets a transaction make is to a collection it creates, which a
    /// transaction older than it does not see at all (its write there is the
    /// create's own conflict, 112 "Collection namespace ... is already in
    /// use").
    pub(crate) fn begin(store: usize, colls: &[(&str, &str)]) -> Self {
        if !crate::ACTIVE_TXN_SESSION.with(|c| c.get()).is_null() {
            return CatalogChange {
                keys: Vec::new(),
                committed: false,
                counted: false,
            };
        }
        let keys: Vec<Key> = colls
            .iter()
            .map(|(d, c)| (store, d.to_string(), c.to_string()))
            .collect();
        {
            let mut marks = MARKS.lock().unwrap_or_else(|e| e.into_inner());
            for k in &keys {
                marks.entry(k.clone()).or_default().0 += 1;
            }
        }
        CATALOG_CHANGES_INFLIGHT.fetch_add(1, Ordering::SeqCst);
        CatalogChange {
            keys,
            committed: false,
            counted: true,
        }
    }

    /// The change is made and visible.
    pub fn committed(mut self) {
        self.committed = true;
    }
}

impl Drop for CatalogChange {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let done = if self.committed {
            Some(CATALOG_CHANGES_DONE.fetch_add(1, Ordering::SeqCst) + 1)
        } else {
            None
        };
        {
            let mut marks = MARKS.lock().unwrap_or_else(|e| e.into_inner());
            for k in &self.keys {
                if let Some(m) = marks.get_mut(k) {
                    m.0 -= 1;
                    if let Some(d) = done {
                        m.1 = m.1.max(d);
                    }
                }
            }
        }
        CATALOG_CHANGES_INFLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Has the catalog entry of `(db, coll)` in `store` changed, or is it
/// changing, since a snapshot taken at `epoch` (see [`catalog_epoch`])?
pub(crate) fn changed_since(store: usize, epoch: u64, db: &str, coll: &str) -> bool {
    if CATALOG_CHANGES_INFLIGHT.load(Ordering::SeqCst) == 0
        && CATALOG_CHANGES_DONE.load(Ordering::SeqCst) <= epoch
    {
        return false;
    }
    let marks = MARKS.lock().unwrap_or_else(|e| e.into_inner());
    [coll, ""].iter().any(|c| {
        marks
            .get(&(store, db.to_string(), c.to_string()))
            .is_some_and(|(inflight, done)| *inflight > 0 || *done > epoch)
    })
}

/// Forget `store`'s marks: it is closing.
pub(crate) fn forget_store(store: usize) {
    MARKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(s, _, _), _| *s != store);
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: usize = 0x320;

    #[test]
    fn a_change_after_the_epoch_is_seen_only_on_its_collection() {
        let epoch = catalog_epoch();
        assert!(!changed_since(STORE, epoch, "rm320_unit", "c"));
        CatalogChange::begin(STORE, &[("rm320_unit", "c")]).committed();
        assert!(changed_since(STORE, epoch, "rm320_unit", "c"));
        assert!(!changed_since(STORE, epoch, "rm320_unit", "d"));
        assert!(!changed_since(STORE, catalog_epoch(), "rm320_unit", "c"));
    }

    #[test]
    fn a_change_in_flight_conflicts_and_a_failed_one_leaves_no_mark() {
        let epoch = catalog_epoch();
        let change = CatalogChange::begin(STORE, &[("rm320_unit_b", "c")]);
        assert!(changed_since(STORE, epoch, "rm320_unit_b", "c"));
        drop(change);
        assert!(!changed_since(STORE, epoch, "rm320_unit_b", "c"));
    }

    #[test]
    fn a_database_wide_change_covers_every_collection() {
        let epoch = catalog_epoch();
        CatalogChange::begin(STORE, &[("rm320_unit_c", "")]).committed();
        assert!(changed_since(STORE, epoch, "rm320_unit_c", "anything"));
        assert!(!changed_since(STORE, epoch, "rm320_unit_d", "anything"));
    }

    #[test]
    fn a_change_in_one_store_is_not_another_stores() {
        let epoch = catalog_epoch();
        CatalogChange::begin(STORE + 1, &[("rm320_unit_e", "c")]).committed();
        assert!(changed_since(STORE + 1, epoch, "rm320_unit_e", "c"));
        assert!(!changed_since(STORE + 2, epoch, "rm320_unit_e", "c"));
        forget_store(STORE + 1);
        assert!(!changed_since(STORE + 1, epoch, "rm320_unit_e", "c"));
    }
}

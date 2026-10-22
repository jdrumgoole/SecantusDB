//! Session state that belongs to the open transaction and ends with it.
//!
//! `PgHandler` used to hold each of these as a field of its own, and every
//! path that ends a transaction (an explicit COMMIT / ROLLBACK, the implicit
//! transaction of an extended-protocol group, a failed group) cleared its own
//! hand-written list of them. The lists drifted: the explicit path never
//! cleared the sequences, so every later block on the connection advanced a
//! sequence inside its transaction and a ROLLBACK undid the `nextval`
//! (RP-050). Grouping the state is what lets one `clear` serve every path.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use bson::Document;
use secantus_pgcatalog::TableDef;

use crate::triggers::DeferredTrigger;

/// A user type created (`Some(doc)`) or dropped (`None`) in the open
/// transaction, overlaid on the committed catalog. Keyed by
/// `(catalog collection, doc _id)`.
pub(crate) type UncommittedTypes = HashMap<(&'static str, String), Option<Document>>;

/// What the open transaction has written to the catalog and not yet
/// committed. Later statements in the SAME transaction consult it on top of
/// the committed catalog; all of it ends with the transaction, whichever
/// way it ends.
#[derive(Default)]
pub(crate) struct PendingCatalog {
    /// Tables created (`Some`) or dropped (`None`) in the open transaction.
    tables: Mutex<HashMap<String, Option<TableDef>>>,
    /// Types created or dropped in the open transaction. A plain read does
    /// not see an uncommitted `CREATE TYPE`, so `CREATE TYPE t ...; SELECT
    /// 't'::regtype` in one transaction needs this overlay. Wrapping the
    /// catalog read in `with_user_transaction` deadlocks COPY (see
    /// `plan_with_session_types`), so this mirrors tables: consulted on top
    /// of the committed catalog and snapshotted by savepoints. A `None`
    /// value is a drop tombstone.
    types: Mutex<UncommittedTypes>,
    /// The sequences whose stored row the open transaction wrote (created,
    /// altered, dropped, reset): `nextval` advances those inside it.
    sequences: Mutex<HashSet<String>>,
}

impl PendingCatalog {
    pub(crate) fn tables(&self) -> MutexGuard<'_, HashMap<String, Option<TableDef>>> {
        self.tables.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn types(&self) -> MutexGuard<'_, UncommittedTypes> {
        self.types.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn sequences(&self) -> MutexGuard<'_, HashSet<String>> {
        self.sequences.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The transaction ended: what it wrote is committed or discarded, so
    /// none of it is pending any more.
    pub(crate) fn clear(&self) {
        self.tables().clear();
        self.types().clear();
        self.sequences().clear();
    }
}

/// What the open transaction did that makes this session's catalog reads
/// bypass the caches every connection shares. Each mark lasts exactly as
/// long as the transaction: left set after it ends, every later statement
/// re-reads what the cache already holds, and every later COMMIT
/// invalidates every other connection's catalog cache -- one committed
/// `CREATE TABLE` block made each later `begin; select 1; commit` on the
/// connection 200 times slower.
#[derive(Default)]
pub(crate) struct TxnMarks {
    /// A statement of the transaction changed the catalog.
    catalog_changed: AtomicBool,
    /// The transaction wrote a role, so its role reads see its own view,
    /// not the shared committed cache.
    roles_written: AtomicBool,
    /// Tables the transaction created or truncated, which COPY FREEZE may
    /// load (`rd_createSubid` / `rd_newRelfilenodeSubid`, at transaction
    /// rather than subtransaction grain).
    /// Each with the savepoint depth it happened at.
    fresh_tables: std::sync::Mutex<Vec<(String, usize)>>,
}

impl TxnMarks {
    pub(crate) fn note_catalog_changed(&self) {
        self.catalog_changed.store(true, Ordering::Relaxed);
    }

    pub(crate) fn note_roles_written(&self) {
        self.roles_written.store(true, Ordering::Relaxed);
    }

    pub(crate) fn note_fresh_table(&self, table: &str, depth: usize) {
        let mut t = self.fresh_tables.lock().unwrap_or_else(|e| e.into_inner());
        t.retain(|(n, _)| n != table);
        t.push((table.to_string(), depth));
    }

    /// Created or truncated in the current subtransaction: at `depth`.
    pub(crate) fn is_fresh_table(&self, table: &str, depth: usize) -> bool {
        self.fresh_tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(n, d)| n == table && *d == depth)
    }

    pub(crate) fn catalog_changed(&self) -> bool {
        self.catalog_changed.load(Ordering::Relaxed)
    }

    pub(crate) fn roles_written(&self) -> bool {
        self.roles_written.load(Ordering::Relaxed)
    }

    /// The transaction ended, committed or not: clears every mark, and
    /// says whether it changed the catalog (a commit publishes that by
    /// bumping the catalog version).
    pub(crate) fn end(&self) -> bool {
        self.roles_written.store(false, Ordering::Relaxed);
        self.fresh_tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.catalog_changed.swap(false, Ordering::Relaxed)
    }
}

/// `SET CONSTRAINTS` for the open transaction: ALL's mode, then each named
/// constraint's (`true` = deferred).
pub(crate) type ConstraintModes = (Option<bool>, HashMap<String, bool>);

/// The open transaction's deferred constraint work: what COMMIT must still
/// check, and the `SET CONSTRAINTS` modes that decide what is deferred. All
/// of it belongs to one transaction. Only ROLLBACK used to clear it, so a
/// `SET CONSTRAINTS ALL DEFERRED` in a committed block went on deferring
/// the checks of every later block on the connection (RP-052).
#[derive(Default)]
pub(crate) struct DeferredChecks {
    /// `(table, constraint name)` of every INITIALLY DEFERRED foreign key
    /// (or deferred unique constraint) a write in the open transaction
    /// touched; re-checked at COMMIT.
    fks: Mutex<Vec<(String, String)>>,
    /// INITIALLY DEFERRED constraint-trigger events queued for COMMIT.
    triggers: Mutex<Vec<DeferredTrigger>>,
    modes: Mutex<ConstraintModes>,
}

impl DeferredChecks {
    pub(crate) fn fks(&self) -> MutexGuard<'_, Vec<(String, String)>> {
        self.fks.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn triggers(&self) -> MutexGuard<'_, Vec<DeferredTrigger>> {
        self.triggers.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn modes(&self) -> MutexGuard<'_, ConstraintModes> {
        self.modes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The transaction ended, committed or not: forget every queued check
    /// and every `SET CONSTRAINTS` mode. A COMMIT runs its checks first.
    pub(crate) fn clear(&self) {
        self.fks().clear();
        self.triggers().clear();
        *self.modes() = (None, HashMap::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_clears_every_mark_and_reports_a_catalog_change() {
        let marks = TxnMarks::default();
        marks.note_catalog_changed();
        marks.note_roles_written();
        assert!(marks.end());
        assert!(!marks.catalog_changed());
        assert!(!marks.roles_written());
        // A transaction that only wrote a role still ends its mark, and has
        // no catalog change to publish.
        marks.note_roles_written();
        assert!(!marks.end());
        assert!(!marks.roles_written());
    }

    #[test]
    fn deferred_clear_forgets_queued_checks_and_set_constraints_modes() {
        let deferred = DeferredChecks::default();
        deferred.fks().push(("t".into(), "t_fk".into()));
        deferred.modes().0 = Some(true);
        deferred.modes().1.insert("t_fk".into(), true);
        deferred.clear();
        assert!(deferred.fks().is_empty());
        assert!(deferred.triggers().is_empty());
        assert_eq!(*deferred.modes(), (None, HashMap::new()));
    }

    #[test]
    fn clear_empties_every_pending_kind() {
        let pending = PendingCatalog::default();
        pending.tables().insert("t".into(), None);
        pending.types().insert(("c", "x".into()), None);
        pending.sequences().insert("s".into());
        pending.clear();
        assert!(pending.tables().is_empty());
        assert!(pending.types().is_empty());
        assert!(pending.sequences().is_empty());
    }
}

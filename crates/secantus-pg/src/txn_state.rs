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
use std::sync::{Mutex, MutexGuard};

use bson::Document;
use secantus_pgcatalog::TableDef;

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

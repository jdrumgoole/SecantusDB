//! The database profiler, as mongod 8.2.11 has it.
//!
//! Each database has a profiling level (0, 1 or 2) and optionally a `filter`.
//! At level 2 every profiled operation on the database is written to its
//! `system.profile`; at level 1 one that ran for `slowms` or longer (subject
//! to `sampleRate`), or, when a filter is set, one the filter matches -- the
//! filter REPLACES the slowms rule, for profiling and for the slow-query log
//! alike, at every level. `slowms` and `sampleRate` are server-wide
//! ([`crate::slowop`]); the level and filter are per database.
//!
//! Measured on 8.2.11 (2026-10-15), and kept that way here:
//!
//! - the settings live in memory: a restart forgets them, and so does
//!   `dropDatabase`;
//! - setting level 1 or 2 creates `system.profile` at once, capped at 1 MB,
//!   and a profiled operation recreates it if it is missing;
//! - `system.profile` is not replicated: neither its creation nor its
//!   documents reach the oplog, so no change stream sees them;
//! - an operation on `system.profile` itself IS profiled (a read of it);
//! - `system.profile` cannot be dropped while the level is not 0 (20).

pub mod filter;
mod record;

pub(crate) use record::{
    begin, finish, mark_reached, note_modified, note_namespace, note_statement_exec, Pending,
    StatementExec, Statements,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use filter::ProfileFilter;

/// The size mongod gives the `system.profile` it creates.
pub const PROFILE_COLLECTION_BYTES: i64 = 1_048_576;

/// The collection the profiler writes.
pub const PROFILE_COLLECTION: &str = "system.profile";

/// One database's profiling settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DbProfile {
    pub level: i32,
    pub filter: Option<Arc<ProfileFilter>>,
}

impl DbProfile {
    fn is_default(&self) -> bool {
        self.level == 0 && self.filter.is_none()
    }
}

/// Every database's profiling settings, shared by every connection of a
/// server. Only databases with a level above 0 or a filter are held, and
/// while none is, the per-operation check is one atomic load.
#[derive(Debug, Default)]
pub struct Profiler {
    held: AtomicUsize,
    dbs: RwLock<HashMap<String, DbProfile>>,
}

impl Profiler {
    pub fn new() -> Self {
        Self::default()
    }

    /// `db`'s settings (level 0 and no filter unless set).
    pub fn settings(&self, db: &str) -> DbProfile {
        self.held(db).unwrap_or_default()
    }

    /// `db`'s settings when they are not the default ones.
    pub fn held(&self, db: &str) -> Option<DbProfile> {
        if self.held.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let dbs = self.dbs.read().unwrap_or_else(|e| e.into_inner());
        dbs.get(db).cloned()
    }

    /// Change `db`'s settings: `level` when given, and the filter when
    /// `filter` is given (`Some(None)` removes it).
    pub fn set(
        &self,
        db: &str,
        level: Option<i32>,
        filter: Option<Option<Arc<ProfileFilter>>>,
    ) -> DbProfile {
        let mut dbs = self.dbs.write().unwrap_or_else(|e| e.into_inner());
        let mut current = dbs.get(db).cloned().unwrap_or_default();
        if let Some(level) = level {
            current.level = level;
        }
        if let Some(filter) = filter {
            current.filter = filter;
        }
        if current.is_default() {
            dbs.remove(db);
        } else {
            dbs.insert(db.to_string(), current.clone());
        }
        self.held.store(dbs.len(), Ordering::Relaxed);
        current
    }

    /// Forget `db`'s settings: `dropDatabase` resets them to the default.
    pub fn forget(&self, db: &str) {
        let mut dbs = self.dbs.write().unwrap_or_else(|e| e.into_inner());
        dbs.remove(db);
        self.held.store(dbs.len(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_settings_off_the_default_are_held() {
        let p = Profiler::new();
        assert_eq!(p.held("a"), None);
        p.set("a", Some(2), None);
        assert_eq!(p.settings("a").level, 2);
        assert_eq!(p.settings("b"), DbProfile::default());
        p.set("a", Some(0), None);
        assert_eq!(p.held.load(Ordering::Relaxed), 0);
        let f = Arc::new(ProfileFilter::parse(&bson::doc! {"op": "query"}).unwrap());
        p.set("a", None, Some(Some(f)));
        assert!(p
            .held("a")
            .is_some_and(|s| s.level == 0 && s.filter.is_some()));
        p.forget("a");
        assert_eq!(p.held("a"), None);
    }
}

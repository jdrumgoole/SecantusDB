//! Per-namespace operation accounting for the `top` command.
//!
//! `top` reports, per `db.collection`, how much time and how many operations
//! each category of work took. Before this existed every `{time, count}` was a
//! hard zero, so `mongotop` rendered an idle server no matter the load.
//!
//! The section mapping was **probed against real mongod 8.3.4**, not assumed --
//! and the obvious assumptions are wrong in four places: `aggregate`, `count`,
//! `distinct` and `findAndModify` all land in `commands`, NOT in
//! `queries`/`update`. mongod's `queries` section is essentially just `find`.
//! Counts are per COMMAND, not per document: a 3-document `insert` bumps the
//! count by 1.

use std::collections::HashMap;
use std::sync::Mutex;

/// The nine sections mongod reports per namespace, in its own order.
pub const TOP_SECTIONS: [&str; 9] = [
    "total",
    "readLock",
    "writeLock",
    "queries",
    "getmore",
    "insert",
    "update",
    "remove",
    "commands",
];

/// Namespaced commands that take a write lock. Everything else falling through
/// to `commands` is a read. Probed: `createIndexes` / `dropIndexes` /
/// `findAndModify` are writeLock; `aggregate` / `count` / `distinct` /
/// `listIndexes` / `explain` are readLock.
const WRITE_COMMANDS: [&str; 10] = [
    "createIndexes",
    "dropIndexes",
    "findAndModify",
    "findandmodify",
    "create",
    "drop",
    "renameCollection",
    "collMod",
    "convertToCapped",
    "emptycapped",
];

/// `(section, lock_kind)` for a command name.
pub fn section_for(name: &str) -> (&'static str, &'static str) {
    match name {
        "find" => ("queries", "readLock"),
        "getMore" => ("getmore", "readLock"),
        "insert" => ("insert", "writeLock"),
        "update" => ("update", "writeLock"),
        "delete" => ("remove", "writeLock"),
        _ => (
            "commands",
            if WRITE_COMMANDS.contains(&name) {
                "writeLock"
            } else {
                "readLock"
            },
        ),
    }
}

/// The collection a command acts on, or `None` when it isn't namespaced.
///
/// For most commands the first key's value IS the collection name. `getMore` is
/// the exception (its value is the cursor id, the namespace rides in
/// `collection`), and `explain` nests the command it explains one level down.
/// Commands with no collection -- `ping`, `hello`, `serverStatus`,
/// `listCollections` -- are not attributed to any namespace, matching mongod.
pub fn namespace_target<'a>(name: &str, doc: &'a bson::Document) -> Option<&'a str> {
    match name {
        "getMore" => doc.get_str("collection").ok(),
        "explain" => doc
            .get_document("explain")
            .ok()
            .and_then(|inner| inner.iter().next())
            .and_then(|(_, v)| v.as_str()),
        _ => doc.get_str(name).ok(),
    }
    .filter(|s| !s.is_empty())
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Slot {
    pub micros: i64,
    pub count: i64,
}

/// Server-wide `top` accounting. Shared behind an `Arc`; a single `Mutex`
/// serialises the writes, which is ample for a counter bump.
#[derive(Debug, Default)]
pub struct TopStats {
    inner: Mutex<HashMap<String, HashMap<&'static str, Slot>>>,
}

impl TopStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accumulate one operation against `namespace` (`db.collection`).
    pub fn record(&self, namespace: &str, name: &str, micros: i64) {
        let micros = micros.max(0);
        let (section, lock_kind) = section_for(name);
        let Ok(mut map) = self.inner.lock() else {
            return; // a poisoned counter must never take the server down
        };
        let entry = map.entry(namespace.to_string()).or_insert_with(|| {
            TOP_SECTIONS
                .iter()
                .map(|s| (*s, Slot::default()))
                .collect::<HashMap<_, _>>()
        });
        for key in ["total", lock_kind, section] {
            let slot = entry.entry(key).or_default();
            slot.micros += micros;
            slot.count += 1;
        }
    }

    /// Drop a namespace's counters. Probed on mongod 8.3.4: dropping a
    /// collection resets its `top` entry rather than accumulating across it.
    pub fn forget(&self, namespace: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(namespace);
        }
    }

    /// Counters for one namespace, or `None` if it has seen no work.
    pub fn snapshot_ns(&self, namespace: &str) -> Option<HashMap<&'static str, Slot>> {
        self.inner.lock().ok()?.get(namespace).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn sections_match_the_mongod_probe() {
        // The four the naive mapping got wrong.
        assert_eq!(section_for("aggregate"), ("commands", "readLock"));
        assert_eq!(section_for("count"), ("commands", "readLock"));
        assert_eq!(section_for("distinct"), ("commands", "readLock"));
        assert_eq!(section_for("findAndModify"), ("commands", "writeLock"));
        // And the ones it got right.
        assert_eq!(section_for("find"), ("queries", "readLock"));
        assert_eq!(section_for("getMore"), ("getmore", "readLock"));
        assert_eq!(section_for("insert"), ("insert", "writeLock"));
        assert_eq!(section_for("update"), ("update", "writeLock"));
        assert_eq!(section_for("delete"), ("remove", "writeLock"));
        assert_eq!(section_for("createIndexes"), ("commands", "writeLock"));
        assert_eq!(section_for("listIndexes"), ("commands", "readLock"));
    }

    #[test]
    fn total_is_read_plus_write() {
        let t = TopStats::new();
        t.record("db.c", "insert", 10);
        t.record("db.c", "find", 5);
        t.record("db.c", "update", 7);
        let ns = t.snapshot_ns("db.c").unwrap();
        assert_eq!(ns["total"].count, 3);
        assert_eq!(ns["readLock"].count, 1);
        assert_eq!(ns["writeLock"].count, 2);
        assert_eq!(ns["total"].micros, 22);
        assert_eq!(ns["insert"].count, 1);
        assert_eq!(ns["queries"].count, 1);
        assert_eq!(ns["update"].count, 1);
        assert_eq!(ns["remove"].count, 0);
    }

    #[test]
    fn forget_resets_a_namespace() {
        let t = TopStats::new();
        t.record("db.c", "insert", 10);
        assert!(t.snapshot_ns("db.c").is_some());
        t.forget("db.c");
        assert!(t.snapshot_ns("db.c").is_none());
    }

    #[test]
    fn namespace_target_handles_the_three_shapes() {
        assert_eq!(namespace_target("find", &doc! {"find": "c"}), Some("c"));
        assert_eq!(
            namespace_target("getMore", &doc! {"getMore": 7_i64, "collection": "c"}),
            Some("c")
        );
        assert_eq!(
            namespace_target("explain", &doc! {"explain": {"find": "c"}}),
            Some("c")
        );
        // Not namespaced.
        assert_eq!(namespace_target("ping", &doc! {"ping": 1}), None);
        assert_eq!(
            namespace_target("listCollections", &doc! {"listCollections": 1}),
            None
        );
        // Empty name is not a namespace.
        assert_eq!(namespace_target("find", &doc! {"find": ""}), None);
    }

    #[test]
    fn negative_elapsed_is_clamped() {
        let t = TopStats::new();
        t.record("db.c", "insert", -5);
        assert_eq!(t.snapshot_ns("db.c").unwrap()["insert"].micros, 0);
    }
}

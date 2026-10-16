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
    /// `serverStatus.opcounters`, in mongod's order: insert, query, update,
    /// delete, getmore, command.
    ops: [std::sync::atomic::AtomicI64; 6],
    /// `serverStatus.network`: requests read, bytes read, bytes written.
    net: [std::sync::atomic::AtomicI64; 3],
    /// `serverStatus.network.compression`, per compressor in
    /// [`COMPRESSORS`] order: compressor bytesIn / bytesOut, then
    /// decompressor bytesIn / bytesOut.
    compression: [[std::sync::atomic::AtomicI64; 4]; 3],
    /// The rest of `serverStatus`'s counters.
    pub status: crate::status::StatusCounters,
}

/// The compressors `serverStatus.network.compression` reports, in mongod
/// 8.2.11's order (its default `networkMessageCompressors`).
pub const COMPRESSORS: [&str; 3] = ["snappy", "zstd", "zlib"];

/// The names of `serverStatus.opcounters`, in mongod's order.
pub const OPCOUNTERS: [&str; 6] = ["insert", "query", "update", "delete", "getmore", "command"];

impl TopStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count a command for `serverStatus.opcounters` the way mongod counts
    /// it (measured 8.2.11): an insert by its documents, an update or delete
    /// by its statements, `find` and `aggregate` as queries, and every other
    /// command as a command.
    pub fn count_op(&self, request: &bson::Document) {
        let Some((name, _)) = request.iter().next() else {
            return;
        };
        let listed = |field: &str| {
            request
                .get_array(field)
                .map_or(1, |items| items.len().max(1) as i64)
        };
        let (slot, by) = match name.as_str() {
            "insert" => (0, listed("documents")),
            "find" | "aggregate" => (1, 1),
            "update" => (2, listed("updates")),
            "delete" => (3, listed("deletes")),
            "getMore" => (4, 1),
            _ => (5, 1),
        };
        self.ops[slot].fetch_add(by, std::sync::atomic::Ordering::Relaxed);
    }

    /// One message read off a connection, of `bytes` with its header.
    pub fn count_request(&self, bytes: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        self.net[0].fetch_add(1, Relaxed);
        self.net[1].fetch_add(bytes as i64, Relaxed);
    }

    /// One message written to a connection.
    pub fn count_reply(&self, bytes: usize) {
        self.net[2].fetch_add(bytes as i64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Requests read, bytes read and bytes written since the server started.
    pub fn network(&self) -> [i64; 3] {
        std::array::from_fn(|i| self.net[i].load(std::sync::atomic::Ordering::Relaxed))
    }

    /// One message body compressed by `compressor` (a [`COMPRESSORS`] name):
    /// `bytes_in` uncompressed bytes became `bytes_out` compressed ones. As
    /// mongod 8.2.11 counts them, neither includes the message header or the
    /// `OP_COMPRESSED` prefix. An unknown name is not counted.
    pub fn count_compressed(&self, compressor: &str, bytes_in: usize, bytes_out: usize) {
        self.count_compression(compressor, 0, bytes_in, bytes_out);
    }

    /// One message body decompressed by `compressor`: `bytes_in` compressed
    /// bytes became `bytes_out` uncompressed ones.
    pub fn count_decompressed(&self, compressor: &str, bytes_in: usize, bytes_out: usize) {
        self.count_compression(compressor, 2, bytes_in, bytes_out);
    }

    fn count_compression(&self, compressor: &str, side: usize, bytes_in: usize, bytes_out: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(i) = COMPRESSORS.iter().position(|c| *c == compressor) else {
            return;
        };
        self.compression[i][side].fetch_add(bytes_in as i64, Relaxed);
        self.compression[i][side + 1].fetch_add(bytes_out as i64, Relaxed);
    }

    /// `serverStatus.network.compression`: for each compressor, in mongod's
    /// order, `{compressor: {bytesIn, bytesOut}, decompressor: {bytesIn,
    /// bytesOut}}` as int64 -- present at zero before any compressed traffic,
    /// as on mongod 8.2.11.
    pub fn compression(&self) -> bson::Document {
        use std::sync::atomic::Ordering::Relaxed;
        let mut out = bson::Document::new();
        for (name, counts) in COMPRESSORS.iter().zip(&self.compression) {
            let n: [i64; 4] = std::array::from_fn(|k| counts[k].load(Relaxed));
            out.insert(
                *name,
                bson::doc! {
                    "compressor": {"bytesIn": n[0], "bytesOut": n[1]},
                    "decompressor": {"bytesIn": n[2], "bytesOut": n[3]},
                },
            );
        }
        out
    }

    /// The six counters, in `OPCOUNTERS` order.
    pub fn opcounters(&self) -> [i64; 6] {
        std::array::from_fn(|i| self.ops[i].load(std::sync::atomic::Ordering::Relaxed))
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
    fn compression_counters_have_mongods_shape() {
        let stats = TopStats::new();
        let zero = doc! {
            "compressor": {"bytesIn": 0_i64, "bytesOut": 0_i64},
            "decompressor": {"bytesIn": 0_i64, "bytesOut": 0_i64},
        };
        // Every compressor, at zero, before any compressed traffic.
        assert_eq!(
            stats.compression(),
            doc! {"snappy": zero.clone(), "zstd": zero.clone(), "zlib": zero.clone()}
        );
        // The deltas mongod 8.2.11 showed for one zlib-compressed ping: 37
        // compressed bytes in became the 35-byte body; the 22-byte reply
        // body went out as 22 compressed bytes.
        stats.count_decompressed("zlib", 37, 35);
        stats.count_compressed("zlib", 22, 22);
        stats.count_compressed("lz4", 1, 1); // not one of mongod's: ignored
        let got = stats.compression();
        assert_eq!(
            got.get_document("zlib").unwrap(),
            &doc! {
                "compressor": {"bytesIn": 22_i64, "bytesOut": 22_i64},
                "decompressor": {"bytesIn": 37_i64, "bytesOut": 35_i64},
            }
        );
        assert_eq!(got.get_document("snappy").unwrap(), &zero);
        assert_eq!(got.get_document("zstd").unwrap(), &zero);
    }

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

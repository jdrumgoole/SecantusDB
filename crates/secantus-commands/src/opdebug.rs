//! What an operation's `Slow query` line and profile entry say beyond its
//! reply: mongod's `OpDebug` (RM-023).
//!
//! Both are made from what the operation counted as it ran -- the keys and
//! documents its plan examined, the index keys its writes inserted and
//! deleted, WiredTiger's per-session statistics, the locks it took -- and
//! from what the command asked for (its read and write concern). mongod
//! prints the same values in two orders, one for the log line
//! (`OpDebug::report`) and one for the profile entry (`OpDebug::append`);
//! [`LINE_ORDER`] and [`PROFILE_ORDER`] are those orders, measured against
//! mongod 8.2.11 field by field (2026-10-27).

use bson::{doc, Bson, Document};

use crate::storage::OperationCounts;
use crate::CommandContext;

/// mongod 8.2.11's order of a `Slow query` line's attributes.
pub(crate) const LINE_ORDER: &[&str] = &[
    "type",
    "isFromUserConnection",
    "ns",
    "collectionType",
    "queryShapeHash",
    "command",
    "originatingCommand",
    "planSummary",
    "planningTimeMicros",
    "resolvedViews",
    "totalOplogSlotDurationMicros",
    "cursorid",
    "keysExamined",
    "docsExamined",
    "hasSortStage",
    "usedDisk",
    "fromMultiPlanner",
    "fromPlanCache",
    "nMatched",
    "nBatches",
    "nModified",
    "ninserted",
    "ndeleted",
    "nUpserted",
    "cursorExhausted",
    "keysInserted",
    "keysDeleted",
    "numYields",
    "nreturned",
    "sortSpills",
    "sortSpilledBytes",
    "sortSpilledRecords",
    "sortSpilledDataStorageSize",
    "sortTotalDataSizeBytes",
    "groupSpills",
    "groupSpilledBytes",
    "groupSpilledRecords",
    "groupSpilledDataStorageSize",
    "planCacheShapeHash",
    "queryHash",
    "planCacheKey",
    "queryFramework",
    "ok",
    "errMsg",
    "errName",
    "errCode",
    "reslen",
    "locks",
    "flowControl",
    "readConcern",
    "writeConcern",
    "waitForWriteConcernDurationMillis",
    "storage",
    "cpuNanos",
    "remote",
    "protocol",
    "numInterruptChecks",
    "queues",
    "workingMillis",
    "durationMillis",
];

/// mongod 8.2.11's order of a profile entry's fields.
pub(crate) const PROFILE_ORDER: &[&str] = &[
    "op",
    "ns",
    "command",
    "originatingCommand",
    "resolvedViews",
    "cursorid",
    "keysExamined",
    "docsExamined",
    "hasSortStage",
    "usedDisk",
    "fromMultiPlanner",
    "fromPlanCache",
    "nMatched",
    "nBatches",
    "nModified",
    "ninserted",
    "ndeleted",
    "nUpserted",
    "cursorExhausted",
    "keysInserted",
    "keysDeleted",
    "numYield",
    "nreturned",
    "sortSpills",
    "sortSpilledBytes",
    "sortSpilledRecords",
    "sortSpilledDataStorageSize",
    "sortTotalDataSizeBytes",
    "groupSpills",
    "groupSpilledBytes",
    "groupSpilledRecords",
    "groupSpilledDataStorageSize",
    "planCacheShapeHash",
    "queryHash",
    "planCacheKey",
    "queryShapeHash",
    "queryFramework",
    "locks",
    "flowControl",
    "readConcern",
    "writeConcern",
    "waitForWriteConcernDuration",
    "storage",
    "ok",
    "errMsg",
    "errName",
    "errCode",
    "responseLength",
    "protocol",
    "cpuNanos",
    "millis",
    "planSummary",
    "planningTimeMicros",
    "totalOplogSlotDurationMicros",
    "execStats",
    "ts",
    "client",
    "appName",
    "allUsers",
    "user",
];

thread_local! {
    /// What the read of the command running on this thread examined, and
    /// its plan's summary, noted by a handler that cannot reach its
    /// context while it reads (`findAndModify`); `dispatch` takes it.
    static EXAMINED: std::cell::RefCell<Option<crate::ExecStats>> =
        const { std::cell::RefCell::new(None) };
}

/// The running command's read examined `keys` and `docs` more, under the
/// plan `summary`.
pub(crate) fn note_examined(keys: i64, docs: i64, summary: Option<String>) {
    EXAMINED.with(|e| {
        let mut e = e.borrow_mut();
        let stats = e.get_or_insert_with(Default::default);
        stats.keys_examined += keys;
        stats.docs_examined += docs;
        if summary.is_some() {
            stats.plan_summary = summary;
        }
    });
}

/// What [`note_examined`] noted for the command, taken.
pub(crate) fn take_examined() -> Option<crate::ExecStats> {
    EXAMINED.with(|e| e.borrow_mut().take())
}

/// The request a command's plan is made for, as `explain` shapes it: a
/// `findAndModify`'s query, sort and hint as a `find`; any other command
/// itself.
pub(crate) fn planned_request(request: &Document) -> std::borrow::Cow<'_, Document> {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    if !matches!(name, "findAndModify" | "findandmodify") {
        return std::borrow::Cow::Borrowed(request);
    }
    let coll = request.get(name).cloned().unwrap_or(Bson::Null);
    let mut find = doc! {
        "find": coll,
        "filter": request.get("query").cloned().unwrap_or_else(|| Bson::Document(Document::new())),
    };
    for key in ["sort", "hint", "collation"] {
        if let Some(v) = request.get(key) {
            find.insert(key, v.clone());
        }
    }
    std::borrow::Cow::Owned(find)
}

/// The CPU time this thread has spent, in nanoseconds, where mongod
/// reports an operation's (`cpuNanos`): on Linux, from the thread's CPU
/// clock. mongod 8.2 reads none elsewhere, and prints none.
pub(crate) fn thread_cpu_nanos() -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid timespec for the call to fill.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        (rc == 0).then(|| ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Whether a command that waited for its write concern wrote anything:
/// mongod reports the wait only then (an insert of a duplicate `_id`
/// alone has none, measured 8.2.11).
pub(crate) fn wrote_something(name: &str, reply: &Document) -> bool {
    if reply.get_f64("ok").unwrap_or(0.0) != 1.0 {
        return false;
    }
    let n = |d: &Document| crate::util::as_i64(d.get("n").unwrap_or(&Bson::Null)).unwrap_or(0);
    match name {
        "insert" | "update" | "delete" => n(reply) > 0,
        "findAndModify" | "findandmodify" => reply
            .get_document("lastErrorObject")
            .is_ok_and(|l| n(l) > 0),
        _ => true,
    }
}

/// Whether an operation that took `locks` wrote: it took the global lock
/// in an exclusive mode.
pub(crate) fn wrote(locks: &Document) -> bool {
    locks.get_document("Global").is_ok_and(|g| {
        g.get_document("acquireCount")
            .is_ok_and(|c| c.contains_key("w") || c.contains_key("W"))
    })
}

/// `doc`'s fields in `order`; any field `order` does not name keeps its
/// place relative to the others, after them.
pub(crate) fn reorder(mut doc: Document, order: &[&str]) -> Document {
    let mut out = Document::new();
    for key in order {
        if let Some(v) = doc.remove(*key) {
            out.insert(*key, v);
        }
    }
    out.extend(doc);
    out
}

/// The `storage` section mongod writes from the operation's WiredTiger
/// statistics (`WiredTigerOperationStats`): the bytes and times of the
/// pages its own thread read and wrote, under `data`, and the time it
/// waited, under `timeWaitingMicros`. A zero is left out, and so is a
/// section, or the whole, with nothing in it (measured 8.2.11). The wait
/// for the cache is `cacheMicros` and for a data handle `handleLock`, as
/// mongod 8.2.11 printed them under eviction and DDL churn (2026-10-27);
/// no probe made mongod wait on the schema lock, and `schemaLock` follows
/// `handleLock`'s spelling. mongod's own waits (`cacheInterruptibleMicros`,
/// which this engine's statistics do not split from the rest of the cache
/// wait, and `storageExecutionMicros`, its execution admission control)
/// are not counted here (RM-355).
pub(crate) fn storage_section(c: &OperationCounts) -> Option<Document> {
    let mut data = Document::new();
    let put = |d: &mut Document, k: &str, v: i64| {
        if v > 0 {
            d.insert(k, v);
        }
    };
    put(&mut data, "bytesRead", c.bytes_read);
    put(&mut data, "bytesWritten", c.bytes_written);
    put(&mut data, "timeReadingMicros", c.read_micros);
    put(&mut data, "timeWritingMicros", c.write_micros);
    put(&mut data, "txnBytesDirty", c.txn_bytes_dirty);
    let mut waiting = Document::new();
    put(&mut waiting, "cacheMicros", c.cache_wait_micros);
    put(&mut waiting, "schemaLock", c.schema_lock_wait_micros);
    put(&mut waiting, "handleLock", c.handle_lock_wait_micros);
    let mut out = Document::new();
    if !data.is_empty() {
        out.insert("data", data);
    }
    if !waiting.is_empty() {
        out.insert("timeWaitingMicros", waiting);
    }
    (!out.is_empty()).then_some(out)
}

/// The read concern the operation ran under, as mongod prints it: the
/// command's own, `provenance: "clientSupplied"`, or the implicit default
/// `{level: "local", provenance: "implicitDefault"}`.
pub(crate) fn read_concern(request: &Document) -> Document {
    match request.get_document("readConcern") {
        Ok(rc) if !rc.is_empty() => {
            let mut out: Document = rc
                .iter()
                .filter(|(k, _)| k.as_str() != "provenance")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !out.contains_key("level") {
                out.insert("level", "local");
            }
            out.insert("provenance", "clientSupplied");
            out
        }
        _ => doc! { "level": "local", "provenance": "implicitDefault" },
    }
}

/// Whether mongod prints a write concern for `name`: the commands that
/// take one, `aggregate` among them whatever its stages.
pub(crate) fn takes_write_concern(name: &str) -> bool {
    takes_write_concern_named(name)
}

/// Whether mongod prints a write concern for `request`: a command that
/// takes one, outside a transaction (a statement of one has its
/// transaction's, given at commit).
pub(crate) fn has_write_concern(request: &Document) -> bool {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    takes_write_concern_named(name) && request.get_bool("autocommit") != Ok(false)
}

fn takes_write_concern_named(name: &str) -> bool {
    matches!(
        name,
        "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "aggregate"
            | "create"
            | "createIndexes"
            | "dropIndexes"
            | "drop"
            | "dropDatabase"
            | "renameCollection"
            | "collMod"
            | "convertToCapped"
            | "cloneCollectionAsCapped"
    )
}

/// The write concern the operation ran under, as mongod prints it: `w`,
/// `wtimeout` (and `j` when given), then its provenance -- the client's,
/// the cluster's stored default (`customDefault`), or mongod's own
/// `{w: "majority", wtimeout: 0}` (`implicitDefault`).
pub(crate) fn write_concern(request: &Document, ctx: &CommandContext) -> Document {
    let (given, provenance) = match request.get_document("writeConcern") {
        Ok(wc) if !wc.is_empty() => (Some(wc.clone()), "clientSupplied"),
        _ => match crate::rwconcern::default_write_concern(ctx) {
            Some(wc) if !wc.is_empty() => (Some(wc), "customDefault"),
            _ => (None, "implicitDefault"),
        },
    };
    let wc = given.unwrap_or_else(|| doc! { "w": "majority", "wtimeout": 0_i32 });
    let mut out = Document::new();
    out.insert("w", wc.get("w").cloned().unwrap_or(Bson::Int32(1)));
    if let Some(j) = wc.get("j") {
        out.insert("j", j.clone());
    }
    let wtimeout = match wc.get("wtimeout") {
        Some(Bson::Int32(n)) => Bson::Int32(*n),
        Some(Bson::Int64(n)) => Bson::Int32(i32::try_from(*n).unwrap_or(i32::MAX)),
        Some(Bson::Double(n)) => Bson::Int32(*n as i32),
        _ => Bson::Int32(0),
    };
    out.insert("wtimeout", wtimeout);
    out.insert("provenance", provenance);
    out
}

/// What kind of collection `db.coll` is, as mongod's line names it:
/// `view`, `timeseries`, or `normal` (a namespace with no collection
/// behind it included, measured 8.2.11).
pub(crate) fn collection_type(ctx: &CommandContext, db: &str, coll: &str) -> &'static str {
    match ctx.storage.as_ref() {
        Some(storage) => collection_type_in(storage.as_ref(), db, coll),
        None => "normal",
    }
}

/// [`collection_type`], asked of `storage`.
pub(crate) fn collection_type_in(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> &'static str {
    if coll.starts_with("$cmd") || coll.is_empty() {
        return "normal";
    }
    if crate::views::is_view(storage, db, coll) {
        return "view";
    }
    if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return "timeseries";
    }
    "normal"
}

/// The client's address and port, as the line's `remote` gives it.
pub(crate) fn remote(ctx: &CommandContext) -> Option<String> {
    let auth = ctx.conn_auth.as_ref()?.lock().ok()?;
    auth.peer_address.clone().filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_storage_section_leaves_out_what_is_zero() {
        assert_eq!(storage_section(&OperationCounts::default()), None);
        let c = OperationCounts {
            bytes_read: 274,
            read_micros: 10,
            bytes_written: 4_201_945,
            write_micros: 2812,
            cache_wait_micros: 2551,
            ..Default::default()
        };
        // mongod 8.2.11's order, from a line that had all of these.
        assert_eq!(
            storage_section(&c),
            Some(doc! {
                "data": {
                    "bytesRead": 274_i64,
                    "bytesWritten": 4_201_945_i64,
                    "timeReadingMicros": 10_i64,
                    "timeWritingMicros": 2812_i64,
                },
                "timeWaitingMicros": { "cacheMicros": 2551_i64 },
            })
        );
    }

    #[test]
    fn a_read_concern_is_the_clients_or_the_implicit_default() {
        assert_eq!(
            read_concern(&doc! {"find": "c"}),
            doc! {"level": "local", "provenance": "implicitDefault"}
        );
        assert_eq!(
            read_concern(&doc! {"find": "c", "readConcern": {"level": "majority"}}),
            doc! {"level": "majority", "provenance": "clientSupplied"}
        );
    }

    #[test]
    fn reorder_puts_known_fields_in_order_and_keeps_the_rest() {
        let d = doc! {"durationMillis": 1, "ns": "a.b", "x": 1, "type": "command"};
        let out = reorder(d, LINE_ORDER);
        let keys: Vec<&str> = out.keys().map(String::as_str).collect();
        assert_eq!(keys, ["type", "ns", "durationMillis", "x"]);
    }
}

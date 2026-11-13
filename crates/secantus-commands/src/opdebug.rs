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
    "delinquencyInfo",
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

/// The running command's write read its documents so (RM-359): its
/// profile entry's `execStats` is made from it once the write is done.
pub(crate) fn note_write_run(run: Option<crate::explain::stream::WriteRun>) {
    EXAMINED.with(|e| {
        let mut e = e.borrow_mut();
        e.get_or_insert_with(Default::default).write_run = run.map(Box::new);
    });
}

/// What [`note_examined`] noted for the command, taken.
pub(crate) fn take_examined() -> Option<crate::ExecStats> {
    EXAMINED.with(|e| e.borrow_mut().take())
}

thread_local! {
    /// How the running command's read chose its plan, as mongod's
    /// `fromMultiPlanner` / `fromPlanCache` say it: noted by the read's
    /// plan cache consultation (`explain::order`), the first one the
    /// command makes.
    static PLANNER: std::cell::Cell<Option<Planned>> = const { std::cell::Cell::new(None) };
}

/// How a read chose its plan: a trial among candidates
/// (`from_multi_planner`), or the plan cache's active plan
/// (`from_plan_cache`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Planned {
    pub from_multi_planner: bool,
    pub from_plan_cache: bool,
}

/// The running command's read chose its plan so; a later read of the same
/// command (a `$lookup`'s) does not replace it.
pub(crate) fn note_planned(planned: Planned) {
    PLANNER.with(|p| {
        if p.get().is_none() {
            p.set(Some(planned));
        }
    });
}

/// What [`note_planned`] noted for the command, taken.
pub(crate) fn take_planned() -> Option<Planned> {
    PLANNER.with(std::cell::Cell::take)
}

thread_local! {
    /// The views the command running on this thread resolved, by name
    /// (RM-362); `dispatch` takes them.
    static RESOLVED_VIEWS: std::cell::RefCell<std::collections::BTreeMap<String, Document>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

/// The running command resolved the namespaces it reads: `own`, the one it
/// names, and every one `pipeline` involves. mongod does this once it has
/// parsed the command's stages and before it parses their expressions, and
/// its line and profile entry name each view it resolved
/// ([`resolved_views`]), whether or not the command then succeeds (measured
/// 8.2.1, 2026-11-07).
pub(crate) fn note_resolved_views(
    storage: &dyn crate::storage::Storage,
    db: &str,
    own: Option<&str>,
    pipeline: &[Bson],
) {
    let found = resolved_views(storage, db, own, pipeline);
    RESOLVED_VIEWS.with(|r| r.borrow_mut().extend(found));
}

/// What [`note_resolved_views`] noted for the command, taken, as mongod's
/// `resolvedViews` prints them: `None` when it resolved no view.
pub(crate) fn take_resolved_views() -> Option<Vec<Bson>> {
    let found = RESOLVED_VIEWS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    (!found.is_empty()).then(|| found.into_values().map(Bson::Document).collect())
}

/// The collection beneath the view `ns` (`<db>.<view>`), when the running
/// read resolved it ([`note_resolved_views`]): mongod runs a read of a view
/// on the collection beneath it, and when the read fails there its line and
/// profile entry keep that namespace -- `<db>.c` for a view `v` on `c`,
/// `<db>.system.buckets.ts` for a time-series collection `ts`. A read that
/// succeeds names the view again; one that failed before it resolved the
/// view names the view (measured 8.2.11, RM-380).
pub(crate) fn beneath_view(ctx: &CommandContext, ns: &str) -> Option<String> {
    let db = ns.split_once('.')?.0;
    ctx.resolved_views
        .as_ref()?
        .iter()
        .filter_map(Bson::as_document)
        .find(|v| v.get_str("viewNamespace") == Ok(ns))
        .and_then(|v| v.get_array("dependencyChain").ok()?.last()?.as_str())
        .map(|base| format!("{db}.{base}"))
}

/// Every view among `own` and the namespaces `pipeline` involves, and among
/// those the views' own pipelines involve, as mongod's `resolvedViews`
/// lists them: `{viewNamespace, dependencyChain, resolvedPipeline}` --
/// the view's name, the names of the chain beneath it down to its
/// collection, and the pipeline it resolved to, its innermost view's stages
/// first -- ordered by name (mongod keeps them in a map). A time-series
/// collection is a view over its buckets. mongod looks up a `$out` or
/// `$merge` target given as `{db, coll}` by its `coll` in the command's own
/// database, and so does this (measured 8.2.1).
pub(crate) fn resolved_views(
    storage: &dyn crate::storage::Storage,
    db: &str,
    own: Option<&str>,
    pipeline: &[Bson],
) -> std::collections::BTreeMap<String, Document> {
    let mut pending: Vec<(String, String)> = Vec::new();
    crate::views::involved_namespaces(pipeline, db, &mut pending);
    pending.extend(own.map(|own| (db.to_string(), own.to_string())));
    let mut out = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    while let Some((_, coll)) = pending.pop() {
        if !seen.insert(coll.clone()) {
            continue;
        }
        let Some((chain, resolved)) = view_chain(storage, db, &coll) else {
            continue;
        };
        crate::views::involved_namespaces(&resolved, db, &mut pending);
        out.insert(
            coll.clone(),
            doc! {
                "viewNamespace": format!("{db}.{coll}"),
                "dependencyChain": chain,
                "resolvedPipeline": resolved,
            },
        );
    }
    out
}

/// The chain of names beneath the view `db.coll`, itself first, and the
/// pipeline it resolves to; `None` when it is no view, or is one past
/// mongod's depth limit or in a cycle (the command that reads it fails).
fn view_chain(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
) -> Option<(Vec<String>, Vec<Bson>)> {
    let mut chain = vec![coll.to_string()];
    let mut pipeline: Vec<Bson> = Vec::new();
    loop {
        let current = chain.last()?.clone();
        let Ok(options) = storage.get_collection_options(db, &current) else {
            break;
        };
        // A namespace this server reads as a view of itself (`system.views`,
        // a time-series collection's buckets) is a collection on mongod.
        if let Some(view_on) = options.get_str("viewOn").ok().filter(|v| *v != current) {
            let mut stages = options
                .get_array("viewPipeline")
                .map(|a| a.to_vec())
                .unwrap_or_default();
            stages.append(&mut pipeline);
            pipeline = stages;
            if chain.iter().any(|c| c == view_on) || chain.len() > crate::views::MAX_VIEW_DEPTH {
                return None;
            }
            chain.push(view_on.to_string());
        } else if let Ok(ts) = options.get_document("timeseries") {
            pipeline.insert(0, Bson::Document(crate::ts_buckets::unpack_stage(ts)));
            chain.push(format!("{}{current}", crate::ts_buckets::BUCKETS_PREFIX));
            break;
        } else {
            break;
        }
    }
    (chain.len() > 1).then_some((chain, pipeline))
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
        // `time_t` and `c_long` are i64 on a 64-bit Linux, narrower on others.
        #[allow(clippy::unnecessary_cast)]
        let nanos = ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64;
        (rc == 0).then_some(nanos)
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

/// An operation's `flowControl` when it took a ticket: `{acquireCount: n}`
/// (RM-357). mongod prints none on the line of an operation that took
/// none, and `{}` in its profile entry when it wrote nothing.
pub(crate) fn flow_control(tickets: i64) -> Option<Document> {
    (tickets > 0).then(|| doc! { "acquireCount": tickets })
}

/// Whether an operation that took `locks` wrote: it took the global lock
/// in an exclusive mode.
pub(crate) fn wrote(locks: &Document) -> bool {
    locks.get_document("Global").is_ok_and(|g| {
        g.get_document("acquireCount")
            .is_ok_and(|c| c.contains_key("w") || c.contains_key("W"))
    })
}

/// How long the operation held oplog slots open before the writes that
/// minted them committed (`totalOplogSlotDurationMicros`, RM-361), as
/// mongod prints it: only when it is more than zero -- a write that minted
/// no entry (an update that changed nothing, a statement of a transaction,
/// whose entries are minted when it commits) or rolled back has none
/// (measured 8.2.11).
pub(crate) fn oplog_slot_micros(c: Option<&OperationCounts>) -> Option<Bson> {
    let micros = c.map_or(0, |c| c.oplog_slot_micros);
    (micros > 0).then(|| append_number(micros))
}

/// `n` as mongod's `BSONObjBuilder::appendNumber` stores a `long long`: an
/// int32 within 2^30 of zero, else an int64.
pub(crate) fn append_number(n: i64) -> Bson {
    const MAX_INT: i64 = 1 << 30;
    if (-MAX_INT..=MAX_INT).contains(&n) {
        Bson::Int32(n as i32)
    } else {
        Bson::Int64(n)
    }
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
/// wait) is not counted here. `storageExecutionMicros` is the time the
/// operation spent acquiring its `execution` tickets, printed whenever it
/// took one (measured 8.2.11: a `find` always has it, `ping` never; RM-355).
pub(crate) fn storage_section(
    c: Option<&OperationCounts>,
    execution: Option<&crate::admission::AdmissionContext>,
) -> Option<Document> {
    let none = OperationCounts::default();
    let c = c.unwrap_or(&none);
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
    put(&mut data, "txnNumUpdates", c.txn_updates);
    let mut waiting = Document::new();
    put(&mut waiting, "cacheMicros", c.cache_wait_micros);
    put(&mut waiting, "schemaLock", c.schema_lock_wait_micros);
    put(&mut waiting, "handleLock", c.handle_lock_wait_micros);
    if let Some(execution) = execution.filter(|e| e.admissions() > 0) {
        waiting.insert("storageExecutionMicros", execution.acquiring_micros());
    }
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

/// The kind a line names a namespace by for its name alone, whatever is
/// behind it and whatever the operation did (measured 8.2.11, RM-478): a
/// `system.` collection is `system` in any database (`admin.system.version`
/// included), and any other namespace on an internal database is named for
/// it -- `admin`, `local`, `config` (`admin.$cmd`, a missing `admin.c`, a
/// failed `find` of one, an `insert` into one). A view or time-series
/// collection on one is still named as such by a read of it that succeeds.
pub(crate) fn namespace_kind(db: &str, coll: &str) -> Option<&'static str> {
    if coll.starts_with("system.") {
        return Some("system");
    }
    match db {
        "admin" => Some("admin"),
        "local" => Some("local"),
        "config" => Some("config"),
        _ => None,
    }
}

/// The kind a line names `db.coll` by: `system` for a `system.`
/// collection; then, when the operation `looked_up` what is behind the
/// name, its [`collection_type`] if that is a `view` or `timeseries` (an
/// `admin.v` view read is `view`); then [`namespace_kind`]; else `normal`.
/// Measured 8.2.11, RM-478.
pub(crate) fn line_collection_type(
    ctx: &CommandContext,
    db: &str,
    coll: &str,
    looked_up: bool,
) -> &'static str {
    let by_name = namespace_kind(db, coll);
    if by_name == Some("system") {
        return "system";
    }
    if looked_up {
        let kind = collection_type(ctx, db, coll);
        if kind != "normal" {
            return kind;
        }
    }
    by_name.unwrap_or("normal")
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
        assert_eq!(
            storage_section(Some(&OperationCounts::default()), None),
            None
        );
        assert_eq!(storage_section(None, None), None);
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
            storage_section(Some(&c), None),
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
    fn an_operation_admitted_to_execution_says_how_long_it_waited() {
        let execution = crate::admission::AdmissionContext::default();
        // Not admitted (`ping`): no wait, and no section.
        assert_eq!(storage_section(None, Some(&execution)), None);
        let holder = std::sync::Arc::new(crate::admission::TicketHolder::new(1, 1, None));
        let execution = std::sync::Arc::new(execution);
        let ticket = holder.try_acquire(&execution);
        assert!(ticket.is_some());
        // Admitted at once: a wait too short to count is still printed.
        assert_eq!(
            storage_section(None, Some(&execution)),
            Some(doc! { "timeWaitingMicros": { "storageExecutionMicros": 0_i64 } })
        );
        execution.record_acquiring(std::time::Duration::from_micros(13));
        let c = OperationCounts {
            bytes_read: 274,
            cache_wait_micros: 5,
            ..Default::default()
        };
        assert_eq!(
            storage_section(Some(&c), Some(&execution)),
            Some(doc! {
                "data": { "bytesRead": 274_i64 },
                "timeWaitingMicros": { "cacheMicros": 5_i64, "storageExecutionMicros": 13_i64 },
            })
        );
    }

    #[test]
    fn a_namespace_on_an_internal_database_or_a_system_one_is_named_for_it() {
        assert_eq!(namespace_kind("admin", "$cmd"), Some("admin"));
        assert_eq!(namespace_kind("admin", "c"), Some("admin"));
        assert_eq!(namespace_kind("local", "oplog.rs"), Some("local"));
        assert_eq!(namespace_kind("config", "c"), Some("config"));
        assert_eq!(namespace_kind("admin", "system.version"), Some("system"));
        assert_eq!(namespace_kind("config", "system.sessions"), Some("system"));
        assert_eq!(namespace_kind("d", "system.js"), Some("system"));
        assert_eq!(namespace_kind("d", "system"), None);
        assert_eq!(namespace_kind("d", "x.system.y"), None);
        assert_eq!(namespace_kind("d", "$cmd"), None);
        assert_eq!(namespace_kind("Admin", "c"), None);
    }

    #[test]
    fn an_oplog_slot_is_shown_only_when_it_was_held() {
        assert_eq!(oplog_slot_micros(None), None);
        assert_eq!(oplog_slot_micros(Some(&OperationCounts::default())), None);
        let c = OperationCounts {
            oplog_slot_micros: 97,
            ..Default::default()
        };
        assert_eq!(oplog_slot_micros(Some(&c)), Some(Bson::Int32(97)));
        assert_eq!(append_number(1 << 30), Bson::Int32(1 << 30));
        assert_eq!(append_number((1 << 30) + 1), Bson::Int64((1 << 30) + 1));
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

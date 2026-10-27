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
    "remote",
    "protocol",
    "numInterruptChecks",
    "queues",
    "cpuNanos",
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
/// section, or the whole, with nothing in it (measured 8.2.11).
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
    put(&mut waiting, "schemaLockMicros", c.schema_lock_wait_micros);
    put(&mut waiting, "handleLockMicros", c.handle_lock_wait_micros);
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

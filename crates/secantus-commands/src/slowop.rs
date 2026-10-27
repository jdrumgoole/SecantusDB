//! Slow-operation logging: the server-wide threshold and the log line.
//!
//! mongod logs every operation that runs for `slowms` or longer (default 100)
//! as a `Slow query` line at severity `I`, whatever the profiling level. The
//! threshold and the sample rate are SERVER-WIDE -- `profile` sets them through
//! any database and every database reads them back (probed on 8.2.11) -- and
//! are not persisted across a restart.

use bson::Document;

use crate::CommandContext;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::Duration;

/// mongod's default `slowms`.
pub const DEFAULT_SLOW_MS: i32 = 100;

/// The server-wide slow-operation settings, shared by every connection.
#[derive(Debug)]
pub struct SlowOpSettings {
    slow_ms: AtomicI32,
    sample_rate_bits: AtomicU64,
}

impl Default for SlowOpSettings {
    fn default() -> Self {
        SlowOpSettings {
            slow_ms: AtomicI32::new(DEFAULT_SLOW_MS),
            sample_rate_bits: AtomicU64::new(1.0_f64.to_bits()),
        }
    }
}

impl SlowOpSettings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn slow_ms(&self) -> i32 {
        self.slow_ms.load(Ordering::Relaxed)
    }

    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.sample_rate_bits.load(Ordering::Relaxed))
    }

    pub fn set(&self, slow_ms: i32, sample_rate: f64) {
        self.slow_ms.store(slow_ms, Ordering::Relaxed);
        self.sample_rate_bits
            .store(sample_rate.to_bits(), Ordering::Relaxed);
    }

    /// Whether an operation that ran for `worked` is to be logged. `sample` is
    /// a uniform draw from `[0, 1)`, taken by the caller so this stays pure.
    /// A negative `slowms` makes every operation slow (mongod 8.2.11 logs
    /// and profiles all of them).
    pub fn should_log(&self, worked: Duration, sample: f64) -> bool {
        if (worked.as_millis() as i128) < i128::from(self.slow_ms()) {
            return false;
        }
        sample < self.sample_rate()
    }
}

/// Where the statements of the running command log themselves: the server's
/// settings, its log and its locks. mongod logs each update and delete
/// statement on a `Slow query` line of its own, component `WRITE`, beside the
/// command's, a `$merge`'s writes included (measured 8.2.11, 2026-10-24).
struct Sink {
    settings: std::sync::Arc<SlowOpSettings>,
    /// The connection the command came in on: the lines' `ctx`.
    connection_id: i64,
    logs: Option<std::sync::Arc<crate::logbuf::LogBuffer>>,
    locks: Option<std::sync::Arc<crate::lock_manager::LockManager>>,
    /// The client's address and port: the lines' `remote`.
    remote: Option<String>,
    /// The server is a member of a replica set, whose lines print the
    /// operation's read concern.
    replicated: bool,
}

thread_local! {
    static SINK: std::cell::RefCell<Option<Sink>> = const { std::cell::RefCell::new(None) };
}

/// Makes the command's slow-operation sink current until dropped.
pub struct SinkScope(Option<Sink>);

/// Enter a command: its statements log through `ctx`'s settings.
pub fn enter(ctx: &CommandContext) -> SinkScope {
    let sink = ctx.slow_ops.as_ref().map(|settings| Sink {
        settings: settings.clone(),
        connection_id: ctx.connection_id,
        logs: ctx.logs.clone(),
        locks: ctx.locks.clone(),
        remote: crate::opdebug::remote(ctx),
        replicated: ctx.replica_set_name.is_some(),
    });
    SinkScope(SINK.with(|s| s.replace(sink)))
}

impl Drop for SinkScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        SINK.with(|s| *s.borrow_mut() = previous);
    }
}

/// Whether a statement of the running command can be logged at all.
pub(crate) fn armed() -> bool {
    SINK.with(|s| s.borrow().is_some())
}

/// The running command's client address and port, for a statement's line.
pub(crate) fn remote() -> Option<String> {
    SINK.with(|s| s.borrow().as_ref().and_then(|s| s.remote.clone()))
}

/// Whether the running command's server is a member of a replica set.
pub(crate) fn replicated() -> bool {
    SINK.with(|s| s.borrow().as_ref().is_some_and(|s| s.replicated))
}

/// mongod's id for a `Slow query` line, whoever writes it.
pub const SLOW_QUERY_ID: i32 = 51803;

/// Log one statement of the running command that ran for `worked`, if that
/// is slow: a `Slow query` line under `component`, and the global IS mongod
/// takes for it (`lock_manager::log_lock`). `attr` is the line's attributes
/// before its durations. An index build's line is its build thread's.
pub(crate) fn statement(component: &'static str, mut attr: Document, worked: Duration) {
    SINK.with(|s| {
        let sink = s.borrow();
        let Some(sink) = sink.as_ref() else {
            return;
        };
        if !sink.settings.should_log(worked, rand::random::<f64>()) {
            return;
        }
        if let Some(lm) = &sink.locks {
            crate::lock_manager::log_lock(lm);
        }
        attr.insert("workingMillis", worked.as_millis() as i64);
        attr.insert("durationMillis", worked.as_millis() as i64);
        let attr = crate::opdebug::reorder(attr, crate::opdebug::LINE_ORDER);
        let thread = if component == "STORAGE" {
            "IndexBuildsCoordinatorMongod-0".to_string()
        } else {
            format!("conn{}", sink.connection_id)
        };
        let entry =
            crate::logbuf::LogEntry::new("I", component, SLOW_QUERY_ID, thread, "Slow query")
                .attr(attr);
        log::info!("{}", entry.render());
        if let Some(logs) = &sink.logs {
            logs.log(entry);
        }
    });
}

/// The reply fields worth a place on the line: what the operation did, or how
/// it failed. Names follow mongod's `Slow query` attributes. An `update` or
/// `delete` reports its counts on each statement's line
/// (`profiler::record`), not the command's.
fn outcome(name: &str, reply: &Document, out: &mut Document) {
    if reply.get_f64("ok").unwrap_or(1.0) != 1.0 {
        failure(reply, out);
        return;
    }
    if matches!(name, "findAndModify" | "findandmodify") {
        return;
    }
    if name == "insert" {
        let n = match reply.get("n") {
            Some(bson::Bson::Int32(n)) => Some(i64::from(*n)),
            Some(bson::Bson::Int64(n)) => Some(*n),
            _ => None,
        };
        if let Some(n) = n {
            out.insert("ninserted", n);
        }
    }
}

/// mongod's error attributes, in its order: `ok: 0`, `errMsg`, `errName`,
/// `errCode` (measured 8.2.11).
pub(crate) fn failure(reply: &Document, out: &mut Document) {
    out.insert("ok", 0_i32);
    out.insert("errMsg", reply.get_str("errmsg").unwrap_or(""));
    let code = reply.get_i32("code").unwrap_or(0);
    let name = match reply.get_str("codeName") {
        Ok(name) => name.to_string(),
        Err(_) => crate::mongod_codes::code_name(code)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Location{code}")),
    };
    out.insert("errName", name);
    out.insert("errCode", code);
}

/// The collection a command names: its own value, or `getMore`'s
/// `collection`.
fn target_collection<'a>(name: &str, request: &'a Document) -> Option<&'a str> {
    let target = if name == "getMore" {
        request.get("collection")
    } else {
        request.get(name)
    };
    match target {
        Some(bson::Bson::String(coll)) if !coll.is_empty() => Some(coll.as_str()),
        _ => None,
    }
}

/// Whether `name` plans a query of its own, so its line has the planner's
/// fields (`planningTimeMicros`).
fn plans(name: &str) -> bool {
    matches!(
        name,
        "find" | "aggregate" | "count" | "distinct" | "findAndModify" | "findandmodify"
    )
}

/// What the line says beyond the command and its outcome (RM-023), each
/// field placed by [`attributes`] in mongod's order: what the plan was and
/// what it examined, as the operation counted them while it ran; the index
/// keys its writes inserted and deleted; the read and write concern it ran
/// under; the locks it took; WiredTiger's statistics for the sessions it
/// used (`storage`); and who asked (`remote`, `protocol`). Measured 8.2.11,
/// 2026-10-27.
pub fn read_attributes(request: &Document, reply: &Document, ctx: &CommandContext) -> Document {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    let mut out = Document::new();
    out.insert("isFromUserConnection", true);
    let coll = target_collection(name, request).unwrap_or("$cmd");
    // mongod names the namespace's kind as the operation found it: what a
    // `create` creates was not there.
    let kind = if name == "create" {
        "normal"
    } else {
        crate::opdebug::collection_type(ctx, &ctx.db_name, coll)
    };
    out.insert("collectionType", kind);
    let ok = reply.get_f64("ok").unwrap_or(1.0) == 1.0;
    let exec = ctx.exec_stats.as_ref().filter(|_| ok);
    if ok {
        // A cursor's `getMore` names the command that opened it, and
        // repeats its plan.
        if let Some(command) = exec.and_then(|e| e.originating_command.clone()) {
            out.insert("originatingCommand", command_shown(&command));
        }
        // A `getMore`'s plan is its cursor's, planned for the command that
        // opened it.
        let shaped = crate::opdebug::planned_request(request);
        let planned = exec
            .and_then(|e| e.originating_command.as_ref())
            .unwrap_or(shaped.as_ref());
        let info = crate::explain::summary::plan_info(planned, ctx);
        let summary = exec.and_then(|e| e.plan_summary.clone()).or(info.summary);
        if let Some(summary) = summary {
            out.insert("planSummary", summary);
        }
        if let Some((shape, key)) = info.hashes {
            out.insert("planCacheShapeHash", shape.clone());
            out.insert("queryHash", shape);
            out.insert("planCacheKey", key);
        }
        // A `findAndModify` prints no engine (measured 8.2.11).
        if let (Some(framework), false) =
            (info.framework, name.eq_ignore_ascii_case("findAndModify"))
        {
            out.insert("queryFramework", framework);
        }
        spills(exec.is_some_and(|e| e.has_sort_stage), &mut out);
        if let (true, Some(micros)) = (plans(name), ctx.planning_micros) {
            out.insert("planningTimeMicros", micros);
        }
        // What the read examined, in mongod's order.
        if let Some(e) = exec {
            if e.cursor_id != 0 {
                out.insert("cursorid", e.cursor_id);
            }
            out.insert("keysExamined", e.keys_examined);
            out.insert("docsExamined", e.docs_examined);
            // A `find` of a view or a time-series collection runs as an
            // `aggregate`, which counts no batches.
            let as_aggregate =
                matches!(out.get_str("collectionType"), Ok("view") | Ok("timeseries"));
            if matches!(name, "find" | "getMore") && !as_aggregate {
                out.insert("nBatches", 1_i64);
            }
            if e.exhausted {
                out.insert("cursorExhausted", true);
            }
        }
        let batch = || -> Option<i64> {
            if let Some(pending) = &ctx.pending_batch {
                return Some(pending.batch.len() as i64);
            }
            let cursor = reply.get_document("cursor").ok()?;
            cursor
                .get_array("firstBatch")
                .or_else(|_| cursor.get_array("nextBatch"))
                .ok()
                .map(|b| b.len() as i64)
        };
        if matches!(name, "findAndModify" | "findandmodify") {
            // What it matched and changed, as its profile entry has it.
            let last = reply.get_document("lastErrorObject").ok();
            let n = last
                .and_then(|l| crate::util::as_i64(l.get("n")?))
                .unwrap_or(0);
            if crate::util::truthy(request.get("remove").unwrap_or(&bson::Bson::Null)) {
                out.insert("ndeleted", n);
            } else {
                let upserted = last.is_some_and(|l| l.contains_key("upserted"));
                out.insert("nMatched", if upserted { 0 } else { n });
                out.insert("nModified", ctx.modified.unwrap_or(0));
                out.insert("nUpserted", i64::from(upserted));
            }
        }
        let returned = match name {
            "find" | "aggregate" | "getMore" => batch(),
            "count" => Some(1),
            "distinct" => reply.get_array("values").ok().map(|v| v.len() as i64),
            "findAndModify" | "findandmodify" => Some(i64::from(!matches!(
                reply.get("value"),
                None | Some(bson::Bson::Null)
            ))),
            _ => None,
        };
        if let Some(n) = returned {
            out.insert("nreturned", n);
        }
    }
    // The index keys the operation's writes inserted and deleted; an
    // `update` or `delete` reports them on each statement's line.
    if !matches!(name, "update" | "delete") {
        if let Some(c) = &ctx.op_counts {
            if let Some(n) = c.keys_inserted {
                out.insert("keysInserted", n);
            }
            if let Some(n) = c.keys_deleted {
                out.insert("keysDeleted", n);
            }
        }
    }
    out.insert("numYields", 0_i64);
    out.insert("reslen", response_length(reply, ctx));
    out.insert("locks", ctx.op_locks.clone().unwrap_or_default());
    // A member of a replica set prints the concerns; a standalone has none
    // (measured 8.2.11).
    let replicated = ctx.replica_set_name.is_some();
    if replicated {
        out.insert("readConcern", crate::opdebug::read_concern(request));
    }
    // A `getMore` runs under the write concern of the `aggregate` that
    // opened its cursor.
    let concerned = match ctx
        .exec_stats
        .as_ref()
        .and_then(|e| e.originating_command.as_ref())
    {
        Some(origin) if name == "getMore" => Some(origin),
        _ => crate::opdebug::takes_write_concern(name).then_some(request),
    };
    if let Some(origin) = concerned.filter(|o| replicated && crate::opdebug::has_write_concern(o)) {
        out.insert("writeConcern", crate::opdebug::write_concern(origin, ctx));
    }
    if let Some(ms) = ctx.write_concern_wait {
        out.insert("waitForWriteConcernDurationMillis", ms);
    }
    if let Some(n) = ctx.cpu_nanos {
        out.insert("cpuNanos", n);
    }
    if let Some(storage) = ctx
        .op_counts
        .as_ref()
        .and_then(crate::opdebug::storage_section)
    {
        out.insert("storage", storage);
    }
    if let Some(remote) = crate::opdebug::remote(ctx) {
        out.insert("remote", remote);
    }
    out.insert("protocol", if ctx.op_query { "op_query" } else { "op_msg" });
    out
}

/// What the operation's sorts and groups did with memory (RM-211, RM-315):
/// `hasSortStage` when its plan sorts in memory, `usedDisk` when a stage
/// spilled, and the spills' counts. mongod's names; this server's spills.
pub(crate) fn spills(has_sort_stage: bool, out: &mut Document) {
    let spilled = secantus_core::memlimit::report();
    if has_sort_stage || spilled.has_sort_stage {
        out.insert("hasSortStage", true);
    }
    if spilled.used_disk {
        out.insert("usedDisk", true);
    }
    if spilled.sort_spills > 0 {
        out.insert("sortSpills", spilled.sort_spills);
        out.insert("sortSpilledBytes", spilled.sort_spilled_bytes);
        out.insert("sortSpilledRecords", spilled.sort_spilled_records);
        out.insert(
            "sortSpilledDataStorageSize",
            spilled.sort_spilled_data_storage_size,
        );
        out.insert("sortTotalDataSizeBytes", spilled.sort_total_data_size);
    }
    if spilled.group_spills > 0 {
        out.insert("groupSpills", spilled.group_spills);
        out.insert("groupSpilledBytes", spilled.group_spilled_bytes);
        out.insert("groupSpilledRecords", spilled.group_spilled_records);
        out.insert(
            "groupSpilledDataStorageSize",
            spilled.group_spilled_data_storage_size,
        );
    }
}

/// The size of the reply on the wire, its document batch included: the
/// same count as a profile entry's `responseLength`
/// (`profiler::record::response_length`).
fn response_length(reply: &Document, ctx: &CommandContext) -> i64 {
    let body = crate::bsondoc::to_vec(reply).map_or(0, |b| b.len());
    let batch = ctx.pending_batch.as_ref().map_or(0, |pending| {
        let items: usize = pending
            .batch
            .iter()
            .enumerate()
            .map(|(i, blob)| 2 + i.to_string().len() + blob.len())
            .sum();
        2 + pending.batch_field.len() + 4 + items + 1
    });
    let frame = if ctx.op_query { 36 } else { 21 };
    (frame + body + batch) as i64
}

/// The command as the line shows it: the request, without an `insert`'s
/// documents (mongod 8.2.11 leaves them out too).
fn command_shown(request: &Document) -> Document {
    request
        .iter()
        .filter(|(k, _)| k.as_str() != "documents")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The attributes of one `Slow query` line. `ns` is `db.collection` for a
/// command that names a collection and `db.$cmd` otherwise, as mongod has
/// it; an `update` or `delete` is `db.$cmd` too, its statements' lines
/// naming the collection. `read` is [`read_attributes`]' part. The whole
/// in mongod's order (`opdebug::LINE_ORDER`).
pub fn attributes(
    request: &Document,
    reply: &Document,
    read: Document,
    db: &str,
    worked: Duration,
    total: Duration,
) -> Document {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    let ns = match target_collection(name, request) {
        Some(_) if matches!(name, "update" | "delete") => format!("{db}.$cmd"),
        Some(coll) => format!("{db}.{coll}"),
        None => format!("{db}.$cmd"),
    };
    let mut attr = Document::new();
    attr.insert("type", "command");
    attr.insert("ns", ns);
    attr.insert("command", command_shown(request));
    outcome(name, reply, &mut attr);
    attr.extend(read);
    attr.insert("workingMillis", worked.as_millis() as i64);
    attr.insert("durationMillis", total.as_millis() as i64);
    crate::opdebug::reorder(attr, crate::opdebug::LINE_ORDER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(attr: Document) -> String {
        bson::Bson::Document(attr)
            .into_relaxed_extjson()
            .to_string()
    }
    use bson::doc;

    #[test]
    fn default_threshold_is_mongods() {
        let s = SlowOpSettings::new();
        assert_eq!(s.slow_ms(), 100);
        assert_eq!(s.sample_rate(), 1.0);
        assert!(!s.should_log(Duration::from_millis(99), 0.0));
        assert!(s.should_log(Duration::from_millis(100), 0.0));
    }

    #[test]
    fn a_zero_threshold_logs_everything_and_a_zero_rate_nothing() {
        let s = SlowOpSettings::new();
        s.set(0, 1.0);
        assert!(s.should_log(Duration::ZERO, 0.999));
        s.set(0, 0.0);
        assert!(!s.should_log(Duration::from_secs(9), 0.0));
        s.set(0, 0.25);
        assert!(s.should_log(Duration::ZERO, 0.2));
        assert!(!s.should_log(Duration::ZERO, 0.3));
        s.set(-1, 1.0);
        assert!(s.should_log(Duration::ZERO, 0.5));
    }

    #[test]
    fn an_insert_line_names_the_collection_and_the_count() {
        let line = json(attributes(
            &doc! {"insert": "t", "documents": [{"payload": "x"}], "$db": "a"},
            &doc! {"n": 100_i32, "ok": 1.0},
            Document::new(),
            "a",
            Duration::from_millis(4140),
            Duration::from_millis(4140),
        ));
        assert_eq!(
            line,
            r#"{"type":"command","ns":"a.t","command":{"insert":"t","$db":"a"},"ninserted":100,"workingMillis":4140,"durationMillis":4140}"#
        );
    }

    #[test]
    fn a_get_more_is_attributed_to_its_collection() {
        let line = json(attributes(
            &doc! {"getMore": 7_i64, "collection": "t", "$db": "a"},
            &doc! {"ok": 1.0},
            Document::new(),
            "a",
            Duration::from_millis(120),
            Duration::from_millis(1120),
        ));
        assert!(
            line.contains(r#""ns":"a.t","command":{"getMore":7,"collection":"t","$db":"a"}"#),
            "{line}"
        );
        assert!(
            line.contains(r#""workingMillis":120,"durationMillis":1120"#),
            "{line}"
        );
    }

    /// mongod 8.2.11's order: the plan after the command, the counts
    /// after the outcome, the times last.
    #[test]
    fn a_read_names_its_plan_and_what_it_returned() {
        let line = json(attributes(
            &doc! {"find": "t", "filter": {"x": {"$gt": 1}}, "$db": "a"},
            &doc! {"ok": 1.0},
            doc! {"planSummary": "IXSCAN { x: 1 }", "nreturned": 3_i64, "reslen": 195_i64},
            "a",
            Duration::from_millis(101),
            Duration::from_millis(101),
        ));
        assert_eq!(
            line,
            r#"{"type":"command","ns":"a.t","command":{"find":"t","filter":{"x":{"$gt":1}},"$db":"a"},"planSummary":"IXSCAN { x: 1 }","nreturned":3,"reslen":195,"workingMillis":101,"durationMillis":101}"#
        );
    }

    #[test]
    fn a_failure_carries_its_code_and_a_collectionless_command_uses_cmd() {
        let line = json(attributes(
            &doc! {"ping": 1_i32, "$db": "a"},
            &doc! {"ok": 0.0, "code": 2_i32, "codeName": "BadValue", "errmsg": "x"},
            Document::new(),
            "a",
            Duration::from_millis(150),
            Duration::from_millis(1150),
        ));
        assert_eq!(
            line,
            r#"{"type":"command","ns":"a.$cmd","command":{"ping":1,"$db":"a"},"ok":0,"errMsg":"x","errName":"BadValue","errCode":2,"workingMillis":150,"durationMillis":1150}"#
        );
    }

    /// mongod 8.2.11's order for a write statement's counts, whatever order
    /// they were put in.
    #[test]
    fn a_line_is_in_mongods_order() {
        let attr = doc! {
            "durationMillis": 1_i64,
            "keysDeleted": 0_i64,
            "keysInserted": 1_i64,
            "nUpserted": 0_i64,
            "storage": {"data": {"bytesRead": 274_i64}},
            "docsExamined": 1_i64,
            "keysExamined": 1_i64,
            "planSummary": "IXSCAN { x: 1 }",
            "nModified": 1_i64,
            "nMatched": 1_i64,
            "collectionType": "normal",
            "ns": "a.c",
            "isFromUserConnection": true,
            "type": "update",
            "numYields": 0_i64,
            "workingMillis": 1_i64,
            "locks": {},
        };
        let keys: Vec<String> = crate::opdebug::reorder(attr, crate::opdebug::LINE_ORDER)
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "type",
                "isFromUserConnection",
                "ns",
                "collectionType",
                "planSummary",
                "keysExamined",
                "docsExamined",
                "nMatched",
                "nModified",
                "nUpserted",
                "keysInserted",
                "keysDeleted",
                "numYields",
                "locks",
                "storage",
                "workingMillis",
                "durationMillis",
            ]
        );
    }
}

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
        attr.insert("durationMillis", worked.as_millis() as i64);
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
/// it failed. Names follow mongod's `Slow query` attributes.
fn outcome(name: &str, reply: &Document, out: &mut Document) {
    let int = |key: &str| match reply.get(key) {
        Some(bson::Bson::Int32(n)) => Some(i64::from(*n)),
        Some(bson::Bson::Int64(n)) => Some(*n),
        _ => None,
    };
    if reply.get_f64("ok").unwrap_or(1.0) != 1.0 {
        out.insert("ok", 0_i32);
        if let Some(code) = int("code") {
            out.insert("errCode", code);
        }
        if let Ok(code_name) = reply.get_str("codeName") {
            out.insert("errName", code_name);
        }
        return;
    }
    if let Some(n) = int("n") {
        let key = match name {
            "insert" => "ninserted",
            "delete" => "ndeleted",
            "update" => "nMatched",
            _ => "n",
        };
        out.insert(key, n);
    }
    if let Some(n) = int("nModified") {
        out.insert("nModified", n);
    }
    if let Ok(errors) = reply.get_array("writeErrors") {
        out.insert("writeErrors", errors.len() as i64);
    }
}

/// What the line says about a read (RM-023): the `planSummary`, how many
/// documents went back (`nreturned`) and the size of the reply (`reslen`),
/// in mongod's order. Measured 8.2.11, 2026-10-16. `keysExamined`,
/// `docsExamined` and the storage wait times are not given: they count the
/// plan's own work, which only a read of the collection would reproduce.
pub fn read_attributes(request: &Document, reply: &Document, ctx: &CommandContext) -> Document {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    let mut out = Document::new();
    if reply.get_f64("ok").unwrap_or(1.0) != 1.0 {
        return out;
    }
    if let Some(summary) = crate::explain::summary::plan_summary(request, ctx) {
        out.insert("planSummary", summary);
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
    let returned = match name {
        "find" | "aggregate" | "getMore" => batch(),
        "count" => Some(1),
        "distinct" => reply.get_array("values").ok().map(|v| v.len() as i64),
        _ => None,
    };
    if let Some(n) = returned {
        out.insert("nreturned", n);
    }
    out.insert("reslen", response_length(reply, ctx));
    out
}

/// The size of the reply on the wire, its document batch included: the
/// same count as a profile entry's `responseLength`
/// (`profiler::record::response_length`).
fn response_length(reply: &Document, ctx: &CommandContext) -> i64 {
    let body = bson::to_vec(reply).map_or(0, |b| b.len());
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
/// it. `read` is [`read_attributes`]' part.
pub fn attributes(
    request: &Document,
    reply: &Document,
    read: Document,
    db: &str,
    worked: Duration,
    total: Duration,
) -> Document {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    // `getMore` names its collection in `collection`; its own value is the
    // cursor id.
    let target = if name == "getMore" {
        request.get("collection")
    } else {
        request.get(name)
    };
    let ns = match target {
        Some(bson::Bson::String(coll)) if !coll.is_empty() => format!("{db}.{coll}"),
        _ => format!("{db}.$cmd"),
    };
    let mut attr = Document::new();
    attr.insert("type", "command");
    attr.insert("ns", ns);
    attr.insert("command", command_shown(request));
    let mut read = read;
    if let Some(summary) = read.remove("planSummary") {
        attr.insert("planSummary", summary);
    }
    outcome(name, reply, &mut attr);
    attr.extend(read);
    attr.insert("workingMillis", worked.as_millis() as i64);
    attr.insert("durationMillis", total.as_millis() as i64);
    attr
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
            r#"{"type":"command","ns":"a.$cmd","command":{"ping":1,"$db":"a"},"ok":0,"errCode":2,"errName":"BadValue","workingMillis":150,"durationMillis":1150}"#
        );
    }
}

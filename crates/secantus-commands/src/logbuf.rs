//! In-process log ring buffer surfaced through `getLog` — the Rust port of
//! `src/secantus/logbuf.py`.
//!
//! Real `mongod` keeps a bounded in-memory log accessible via
//! `db.adminCommand({getLog: "global"})`. The Rust server's `getLog` used to
//! return an empty array; this gives it a backing store the server appends to
//! (connection accepts, …) and [`crate::serverinfo::get_log`] reads. Pure
//! module — no I/O outside the in-memory deque.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use bson::spec::BinarySubtype;
use bson::{Bson, Document};

/// Default retained-entry cap. Older entries drop when full.
const DEFAULT_CAPACITY: usize = 5000;

/// How many of the latest lines `getLog` returns: mongod's ring holds 1024
/// and answers with 1023 of them (measured 8.2.11, 2026-10-24).
pub const GET_LOG_LINES: usize = 1023;

/// One line of mongod's structured log: `{t, s, c, id, ctx, msg, attr?,
/// tags?}`.
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// When the line was recorded.
    pub ts: bson::DateTime,
    /// mongod severity: `"I"` / `"W"` / `"E"` / `"D1"` ...
    pub severity: &'static str,
    /// mongod component: `"NETWORK"` / `"COMMAND"` / `"-"` (the default) ...
    pub component: &'static str,
    /// mongod's id for the message (`LOGV2(id, ...)`): 51803 is a `Slow
    /// query` line wherever it is written.
    pub id: i32,
    /// The thread that wrote it: `conn12`, `listener`, `initandlisten`.
    pub ctx: String,
    /// The message.
    pub msg: String,
    /// The message's attributes, when it has any.
    pub attr: Option<Document>,
    /// The logs the line also belongs to (`startupWarnings`).
    pub tags: Vec<&'static str>,
}

impl LogEntry {
    /// A line at `severity`, under `component`, with mongod's `id`.
    pub fn new(
        severity: &'static str,
        component: &'static str,
        id: i32,
        ctx: impl Into<String>,
        msg: impl Into<String>,
    ) -> Self {
        LogEntry {
            ts: bson::DateTime::now(),
            severity,
            component,
            id,
            ctx: ctx.into(),
            msg: msg.into(),
            attr: None,
            tags: Vec::new(),
        }
    }

    /// The line with its attributes.
    pub fn attr(mut self, attr: Document) -> Self {
        self.attr = Some(attr);
        self
    }

    /// The line as mongod's JSON formatter writes it, column padding
    /// included: `{"t":{"$date":"..."},"s":"I",  "c":"COMMAND",  "id":51803,
    /// "ctx":"conn8","msg":"Slow query","attr":{...}}` (measured 8.2.11,
    /// 2026-10-24: `"s"` padded to 10 columns, `"c"` to 16, `"id"` to 14).
    pub fn render(&self) -> String {
        let quoted = |s: &str| {
            Bson::String(s.to_string())
                .into_relaxed_extjson()
                .to_string()
        };
        let mut line = format!(
            "{{\"t\":{{\"$date\":\"{}\"}},",
            secantus_core::logtime::log_timestamp(self.ts.timestamp_millis())
        );
        line.push_str(&format!("{:<10}", format!("\"s\":\"{}\",", self.severity)));
        line.push_str(&format!("{:<16}", format!("\"c\":\"{}\",", self.component)));
        line.push_str(&format!("{:<14}", format!("\"id\":{},", self.id)));
        line.push_str(&format!(
            "\"ctx\":{},\"msg\":{}",
            quoted(&self.ctx),
            quoted(&self.msg)
        ));
        if let Some(attr) = &self.attr {
            line.push_str(",\"attr\":");
            line.push_str(&log_json(attr));
        }
        if !self.tags.is_empty() {
            let tags: Vec<String> = self.tags.iter().map(|t| quoted(t)).collect();
            line.push_str(&format!(",\"tags\":[{}]", tags.join(",")));
        }
        line.push('}');
        line
    }
}

/// A document as mongod's log writes it: relaxed Extended JSON, except that
/// a UUID is `{"$uuid": "..."}`.
pub fn log_json(doc: &Document) -> String {
    fn uuids(v: &Bson) -> Bson {
        match v {
            Bson::Binary(b) if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 => {
                let h: String = b.bytes.iter().map(|x| format!("{x:02x}")).collect();
                let text = format!(
                    "{}-{}-{}-{}-{}",
                    &h[0..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..32]
                );
                let mut d = Document::new();
                d.insert("$uuid", text);
                Bson::Document(d)
            }
            Bson::Document(d) => {
                Bson::Document(d.iter().map(|(k, v)| (k.clone(), uuids(v))).collect())
            }
            Bson::Array(a) => Bson::Array(a.iter().map(uuids).collect()),
            other => other.clone(),
        }
    }
    uuids(&Bson::Document(doc.clone()))
        .into_relaxed_extjson()
        .to_string()
}

/// Thread-safe bounded ring buffer of [`LogEntry`]; older entries are dropped
/// when the buffer is full. Poison-tolerant so a panicked writer can't wedge it.
pub struct LogBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    capacity: usize,
    /// Every line ever written: `getLog`'s `totalLinesWritten`.
    written: AtomicU64,
}

impl LogBuffer {
    /// A buffer with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// A buffer retaining at most `capacity` entries (clamped to `>= 1`).
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        LogBuffer {
            entries: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity,
            written: AtomicU64::new(0),
        }
    }

    /// Append one line.
    pub fn log(&self, entry: LogEntry) {
        let mut q = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(entry);
        self.written.fetch_add(1, Ordering::Relaxed);
    }

    /// How many lines have ever been written.
    pub fn total_written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// The most recent `n` entries (oldest-first); `None` = all.
    pub fn tail(&self, n: Option<usize>) -> Vec<LogEntry> {
        let q = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match n {
            Some(n) if n < q.len() => q.iter().skip(q.len() - n).cloned().collect(),
            _ => q.iter().cloned().collect(),
        }
    }

    /// Number of retained entries.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the buffer holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_tail_are_oldest_first() {
        let buf = LogBuffer::new();
        buf.log(LogEntry::new("I", "NETWORK", 1, "conn1", "one"));
        buf.log(LogEntry::new("I", "NETWORK", 1, "conn1", "two"));
        let all = buf.tail(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].msg, "one");
        assert_eq!(all[1].msg, "two");
        assert_eq!(buf.tail(Some(1)).len(), 1);
        assert_eq!(buf.tail(Some(1))[0].msg, "two");
    }

    #[test]
    fn capacity_drops_oldest() {
        let buf = LogBuffer::with_capacity(2);
        for msg in ["a", "b", "c"] {
            buf.log(LogEntry::new("I", "NETWORK", 1, "conn1", msg));
        }
        assert_eq!(buf.total_written(), 3);
        let all = buf.tail(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].msg, "b");
        assert_eq!(all[1].msg, "c");
    }

    /// mongod 8.2.11's padding, measured 2026-10-24.
    #[test]
    fn renders_as_mongod_s_formatter_does() {
        let mut doc = bson::Document::new();
        doc.insert(
            "id",
            bson::Binary {
                subtype: BinarySubtype::Uuid,
                bytes: vec![0x11; 16],
            },
        );
        let line = LogEntry::new("I", "WRITE", 51803, "conn8", "Slow query")
            .attr(bson::doc! {"type": "update", "lsid": doc})
            .render();
        let after_t = &line[line.find("},").unwrap() + 2..];
        assert!(
            after_t.starts_with(
                "\"s\":\"I\",  \"c\":\"WRITE\",    \"id\":51803,   \"ctx\":\"conn8\",\"msg\":\"Slow query\","
            ),
            "{line}"
        );
        assert!(
            line.ends_with(
                "\"attr\":{\"type\":\"update\",\"lsid\":{\"id\":{\"$uuid\":\"11111111-1111-1111-1111-111111111111\"}}}}"
            ),
            "{line}"
        );
    }
}

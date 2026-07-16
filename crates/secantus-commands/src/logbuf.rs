//! In-process log ring buffer surfaced through `getLog` — the Rust port of
//! `src/secantus/logbuf.py`.
//!
//! Real `mongod` keeps a bounded in-memory log accessible via
//! `db.adminCommand({getLog: "global"})`. The Rust server's `getLog` used to
//! return an empty array; this gives it a backing store the server appends to
//! (connection accepts, …) and [`crate::diagnostics::get_log`] reads. Pure
//! module — no I/O outside the in-memory deque.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Default retained-entry cap (matches the Python `LogBuffer`). Older entries
/// drop when full.
const DEFAULT_CAPACITY: usize = 5000;

/// One line in the in-memory log.
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// When the line was recorded.
    pub ts: bson::DateTime,
    /// mongod severity letter: `"I"` / `"W"` / `"E"` / `"D"`.
    pub level: String,
    /// mongod component: `"NETWORK"` / `"COMMAND"` / `"STORAGE"` / …
    pub component: String,
    /// The human-readable message.
    pub msg: String,
}

/// Thread-safe bounded ring buffer of [`LogEntry`]; older entries are dropped
/// when the buffer is full. Poison-tolerant so a panicked writer can't wedge it.
pub struct LogBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    capacity: usize,
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
        }
    }

    /// Append one line with mongod-style `level` / `component` tags.
    pub fn append(&self, level: &str, component: &str, msg: impl Into<String>) {
        let entry = LogEntry {
            ts: bson::DateTime::now(),
            level: level.to_string(),
            component: component.to_string(),
            msg: msg.into(),
        };
        let mut q = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(entry);
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
        buf.append("I", "NETWORK", "one");
        buf.append("I", "NETWORK", "two");
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
        buf.append("I", "NETWORK", "a");
        buf.append("I", "NETWORK", "b");
        buf.append("I", "NETWORK", "c");
        let all = buf.tail(None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].msg, "b");
        assert_eq!(all[1].msg, "c");
    }
}

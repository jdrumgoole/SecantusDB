//! Slow-operation logging: the server-wide threshold and the log line.
//!
//! mongod logs every operation that runs for `slowms` or longer (default 100)
//! as a `Slow query` line at severity `I`, whatever the profiling level. The
//! threshold and the sample rate are SERVER-WIDE -- `profile` sets them through
//! any database and every database reads them back (probed on 8.2.11) -- and
//! are not persisted across a restart.

use bson::Document;
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
    pub fn should_log(&self, worked: Duration, sample: f64) -> bool {
        let slow_ms = self.slow_ms();
        if slow_ms < 0 || worked.as_millis() < slow_ms as u128 {
            return false;
        }
        sample < self.sample_rate()
    }
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

/// The attributes of one `Slow query` line, as relaxed Extended JSON. `ns` is
/// `db.collection` for a command that names a collection and `db.$cmd`
/// otherwise, as mongod has it. The command's own documents are left out: an
/// `insert` of a thousand documents is not a log line.
pub fn attributes(
    request: &Document,
    reply: &Document,
    db: &str,
    conn_id: i64,
    worked: Duration,
    total: Duration,
) -> String {
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
    attr.insert("command", name);
    attr.insert("ctx", format!("conn{conn_id}"));
    outcome(name, reply, &mut attr);
    attr.insert("workingMillis", worked.as_millis() as i64);
    attr.insert("durationMillis", total.as_millis() as i64);
    bson::Bson::Document(attr)
        .into_relaxed_extjson()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }

    #[test]
    fn an_insert_line_names_the_collection_and_the_count() {
        let line = attributes(
            &doc! {"insert": "t", "documents": [{"payload": "x"}], "$db": "a"},
            &doc! {"n": 100_i32, "ok": 1.0},
            "a",
            7,
            Duration::from_millis(4140),
            Duration::from_millis(4140),
        );
        assert_eq!(
            line,
            r#"{"type":"command","ns":"a.t","command":"insert","ctx":"conn7","ninserted":100,"workingMillis":4140,"durationMillis":4140}"#
        );
    }

    #[test]
    fn a_get_more_is_attributed_to_its_collection() {
        let line = attributes(
            &doc! {"getMore": 7_i64, "collection": "t", "$db": "a"},
            &doc! {"ok": 1.0},
            "a",
            2,
            Duration::from_millis(120),
            Duration::from_millis(1120),
        );
        assert!(line.contains(r#""ns":"a.t","command":"getMore""#), "{line}");
        assert!(
            line.contains(r#""workingMillis":120,"durationMillis":1120"#),
            "{line}"
        );
    }

    #[test]
    fn a_failure_carries_its_code_and_a_collectionless_command_uses_cmd() {
        let line = attributes(
            &doc! {"ping": 1_i32, "$db": "a"},
            &doc! {"ok": 0.0, "code": 2_i32, "codeName": "BadValue", "errmsg": "x"},
            "a",
            1,
            Duration::from_millis(150),
            Duration::from_millis(1150),
        );
        assert_eq!(
            line,
            r#"{"type":"command","ns":"a.$cmd","command":"ping","ctx":"conn1","ok":0,"errCode":2,"errName":"BadValue","workingMillis":150,"durationMillis":1150}"#
        );
    }
}

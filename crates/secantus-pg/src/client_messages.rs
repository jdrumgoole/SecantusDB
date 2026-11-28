//! Which messages below ERROR reach the client: `client_min_messages`.
//!
//! PostgreSQL decides in `errstart` (elog.c `should_output_to_client`), at
//! the moment a message is raised: it goes to the client when its level is
//! at or above `client_min_messages`, and an INFO always does. So the value
//! in force at the raise is the one that counts -- a function's own `SET
//! client_min_messages = warning` withholds the notices raised inside it,
//! and a DO block that changes the setting part-way sends what it raised
//! before. Every notice this server queues for the client goes through
//! [`PgHandler::queue_notice`], which makes that decision there and then.

use super::*;

/// elog.h's message levels.
const DEBUG5: i32 = 10;
const DEBUG4: i32 = 11;
const DEBUG3: i32 = 12;
const DEBUG2: i32 = 13;
const DEBUG1: i32 = 14;
const LOG: i32 = 15;
const INFO: i32 = 17;
const NOTICE: i32 = 18;
const WARNING: i32 = 19;
const ERROR: i32 = 21;

/// The level a `client_min_messages` value names (`client_message_level_options`
/// in guc.c; `debug` is stored as `debug2`). An unknown value -- none can be
/// stored -- reads as the default, `notice`.
fn min_level(value: &str) -> i32 {
    match value {
        "debug5" => DEBUG5,
        "debug4" => DEBUG4,
        "debug3" => DEBUG3,
        "debug2" | "debug" => DEBUG2,
        "debug1" => DEBUG1,
        "log" => LOG,
        "info" => INFO,
        "warning" => WARNING,
        "error" => ERROR,
        _ => NOTICE,
    }
}

/// The level of a message by its severity. The only DEBUG message this
/// server raises is a `RAISE DEBUG`, which PL/pgSQL raises at DEBUG1.
fn message_level(severity: &str) -> i32 {
    match severity {
        "DEBUG" => DEBUG1,
        "LOG" => LOG,
        "INFO" => INFO,
        "NOTICE" => NOTICE,
        "WARNING" => WARNING,
        _ => ERROR,
    }
}

/// `should_output_to_client`, for a session past authentication.
fn reaches_client(severity: &str, client_min_messages: &str) -> bool {
    let level = message_level(severity);
    level == INFO || level >= min_level(client_min_messages)
}

impl PgHandler {
    /// Queue a message below ERROR for the client of the statement in
    /// flight, unless `client_min_messages` withholds it now. Answers
    /// whether it was queued.
    pub(crate) fn queue_notice(&self, info: ErrorInfo) -> bool {
        if !self.notice_reaches_client(&info.severity) {
            return false;
        }
        self.pending_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(info);
        true
    }

    /// Would a message of this severity reach the client now?
    pub(crate) fn notice_reaches_client(&self, severity: &str) -> bool {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let min = settings
            .get("client_min_messages")
            .map(String::as_str)
            .unwrap_or("notice");
        reaches_client(severity, min)
    }
}

#[cfg(test)]
mod tests {
    use super::reaches_client;

    #[test]
    fn a_message_reaches_the_client_at_or_above_the_minimum() {
        // The default sends NOTICE and up, never DEBUG or LOG.
        assert!(reaches_client("NOTICE", "notice"));
        assert!(reaches_client("WARNING", "notice"));
        assert!(!reaches_client("LOG", "notice"));
        assert!(!reaches_client("DEBUG", "notice"));
        // `warning` withholds a NOTICE; `error` a WARNING too.
        assert!(!reaches_client("NOTICE", "warning"));
        assert!(reaches_client("WARNING", "warning"));
        assert!(!reaches_client("WARNING", "error"));
        // LOG sits below NOTICE for the client; a RAISE DEBUG is DEBUG1.
        assert!(reaches_client("LOG", "log"));
        assert!(!reaches_client("DEBUG", "log"));
        assert!(reaches_client("DEBUG", "debug1"));
        assert!(reaches_client("DEBUG", "debug5"));
        // `debug` is stored as `debug2`, below DEBUG1.
        assert!(reaches_client("DEBUG", "debug2"));
        assert!(!reaches_client("LOG", "warning"));
    }

    #[test]
    fn an_info_always_reaches_the_client() {
        for min in ["debug5", "log", "notice", "warning", "error"] {
            assert!(reaches_client("INFO", min), "{min}");
        }
        // `info` itself (a hidden spelling) withholds LOG and sends NOTICE.
        assert!(!reaches_client("LOG", "info"));
        assert!(reaches_client("NOTICE", "info"));
    }
}

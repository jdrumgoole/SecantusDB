//! Server-wide `configureFailPoint` registry — a port of the Python server's
//! `secantus.failpoints`.
//!
//! Real `mongod` exposes a debug `configureFailPoint` command that the driver
//! test suites lean on heavily: "set a `failCommand` failpoint that fails
//! `getMore` with code 100, then prove the driver surfaces / retries it." Only
//! the `failCommand` slice those tests exercise is implemented; other failpoint
//! names are accepted-but-ignored so test setup doesn't hit `CommandNotFound`.
//!
//! Applied in `dispatch` before the real handler runs:
//! * `mode: "alwaysOn"` → fires until disabled; `{times: N}` → next N matches;
//!   `{skip: N, times: M}` → skip N then fire M; `"off"` → disabled.
//! * `data.errorCode` → the matched command short-circuits with `{ok: 0, code}`.
//! * `data.writeConcernError` → the command runs, then the block is attached.
//! * `data.blockConnection` + `blockTimeMS` → sleep before processing (drivers'
//!   CSOT tests rely on this to trip a client-side timeout).
//! * `data.closeConnection` → recorded; the server layer drops the socket.
//! An optional `failCommands: [...]` filters by command name (empty == any).

use std::sync::Mutex;

use bson::{Bson, Document};

use crate::util::as_i64;

/// One configured `failCommand` failpoint.
struct FailCommand {
    fail_commands: Vec<String>,
    error_code: Option<i32>,
    error_labels: Vec<String>,
    write_concern_error: Option<Document>,
    close_connection: bool,
    block_time_ms: i64,
    /// `None` == `alwaysOn`; an int counts down to zero.
    times_remaining: Option<i64>,
    skip_remaining: i64,
}

/// The decision the registry returns for a single command.
#[derive(Clone, Default)]
pub struct FailPointMatch {
    pub error_code: Option<i32>,
    pub error_labels: Vec<String>,
    pub write_concern_error: Option<Document>,
    pub close_connection: bool,
    pub block_time_ms: i64,
}

/// Thread-safe per-server registry of active failpoints.
#[derive(Default)]
pub struct FailPointRegistry {
    inner: Mutex<Vec<FailCommand>>,
}

impl FailPointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install / replace / disable a named failpoint. `mode` is what mongod
    /// accepts (`"alwaysOn"` / `"off"` / `{times}` / `{skip, times}`).
    pub fn configure(&self, name: &str, mode: &Bson, data: &Document) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // A new failCommand replaces any prior one; other failpoint names are
        // accept-but-ignore (mongod exposes dozens we don't model).
        if name != "failCommand" {
            return;
        }
        g.clear();
        let (times_remaining, skip_remaining) = match mode {
            Bson::String(s) if s == "alwaysOn" => (None, 0),
            Bson::String(s) if s == "off" => return,
            Bson::Document(m) => {
                let times = m.get("times").and_then(as_i64);
                let skip = m.get("skip").and_then(as_i64).unwrap_or(0);
                (times, skip)
            }
            _ => return,
        };
        let fail_commands = data
            .get_array("failCommands")
            .ok()
            .map(|a| {
                a.iter()
                    .filter_map(|b| b.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let error_code = data.get("errorCode").and_then(as_i64).map(|n| n as i32);
        let error_labels = data
            .get_array("errorLabels")
            .ok()
            .map(|a| {
                a.iter()
                    .filter_map(|b| b.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let write_concern_error = data.get_document("writeConcernError").ok().cloned();
        let close_connection = data.get_bool("closeConnection").unwrap_or(false);
        let block_connection = data.get_bool("blockConnection").unwrap_or(false);
        let block_time_ms = if block_connection {
            data.get("blockTimeMS").and_then(as_i64).unwrap_or(0)
        } else {
            0
        };
        // Nothing actionable -> don't install (matches mongod's no-op).
        if error_code.is_none()
            && write_concern_error.is_none()
            && !close_connection
            && block_time_ms == 0
        {
            return;
        }
        g.push(FailCommand {
            fail_commands,
            error_code,
            error_labels,
            write_concern_error,
            close_connection,
            block_time_ms,
            times_remaining,
            skip_remaining,
        });
    }

    /// The decision for one incoming command `name`, consuming a `times`/`skip`
    /// budget. `None` means no failpoint applies.
    pub fn match_command(&self, name: &str) -> Option<FailPointMatch> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for fc in g.iter_mut() {
            if !fc.fail_commands.is_empty() && !fc.fail_commands.iter().any(|c| c == name) {
                continue;
            }
            if fc.skip_remaining > 0 {
                fc.skip_remaining -= 1;
                continue;
            }
            match fc.times_remaining.as_mut() {
                Some(0) => continue,
                Some(n) => *n -= 1,
                None => {}
            }
            return Some(FailPointMatch {
                error_code: fc.error_code,
                error_labels: fc.error_labels.clone(),
                write_concern_error: fc.write_concern_error.clone(),
                close_connection: fc.close_connection,
                block_time_ms: fc.block_time_ms,
            });
        }
        None
    }
}

/// A `codeName` for a failpoint-injected error code — the well-known mongod
/// names the driver retry/label logic keys on, else a generic `Location<code>`.
pub fn fail_code_name(code: i32) -> String {
    match code {
        6 => "HostUnreachable",
        7 => "HostNotFound",
        89 => "NetworkTimeout",
        91 => "ShutdownInProgress",
        100 => "CannotSatisfyWriteConcern",
        134 => "ReadConcernMajorityNotAvailableYet",
        189 => "PrimarySteppedDown",
        262 => "ExceededTimeLimit",
        9001 => "SocketException",
        10107 => "NotWritablePrimary",
        11600 => "InterruptedAtShutdown",
        11602 => "InterruptedDueToReplStateChange",
        13435 => "NotPrimaryNoSecondaryOk",
        13436 => "NotPrimaryOrSecondary",
        _ => return format!("Location{code}"),
    }
    .to_string()
}

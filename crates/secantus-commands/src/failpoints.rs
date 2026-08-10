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
//!
//! An optional `failCommands: [...]` filters by command name (empty == any).

use std::sync::Mutex;

use bson::{Bson, Document};

use crate::util::as_i64;

/// One configured `failCommand` failpoint.
struct FailCommand {
    fail_commands: Vec<String>,
    /// Configured as `failGetMoreAfterCursorCheckout` rather than
    /// `failCommand`. mongod injects that one *inside* the change-stream
    /// getMore path, where it stamps `ResumableChangeStreamError` on a
    /// resumable code; `failCommand` short-circuits earlier and carries only
    /// the labels the failpoint itself specified. The change-streams spec
    /// pins the difference: `failGetMoreAfterCursorCheckout` + code 6 resumes,
    /// `failCommand` + code 6 does not.
    server_injected: bool,
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
    /// See `FailCommand::server_injected`.
    pub server_injected: bool,
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
        // A new failpoint replaces any prior one; names we don't model are
        // accept-but-ignore (mongod exposes dozens).
        //
        // `failGetMoreAfterCursorCheckout` is `failCommand` scoped to `getMore`
        // — mongod fails the getMore once the cursor has been checked out, with
        // the supplied errorCode. Drivers use it to provoke a *resumable* error
        // mid-stream and assert the change stream resumes; libmongoc's
        // `_setup_for_resume` reaches for it on wire >= 4.4 (older servers get
        // the plain `failCommand` form). Ignoring it meant the getMore
        // succeeded, no error was raised, and no resume ever happened.
        let getmore_only = name == "failGetMoreAfterCursorCheckout";
        if name != "failCommand" && !getmore_only {
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
        let fail_commands: Vec<String> = if getmore_only {
            vec!["getMore".to_string()]
        } else {
            data.get_array("failCommands")
                .ok()
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let error_code = data.get("errorCode").and_then(as_i64).map(|n| n as i32);
        let server_injected = getmore_only;
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
            server_injected,
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
                server_injected: fc.server_injected,
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
/// Error codes mongod classifies as resumable for a change stream
/// (`ErrorCodes::isResumableChangeStreamError`). On one of these it stamps the
/// reply with `ResumableChangeStreamError`, and drivers on wire >= 9 resume on
/// that label alone — never on the bare code. Pinned by the change-streams
/// unified spec `change-streams-resume-errorLabels`, which walks every one.
/// Note `MaxTimeMSExpired` (50) is deliberately absent: the spec resumes on it
/// only when the failpoint sets the label explicitly.
pub const RESUMABLE_CHANGE_STREAM_CODES: &[i32] = &[
    6,     // HostUnreachable
    7,     // HostNotFound
    63,    // StaleShardVersion
    89,    // NetworkTimeout
    91,    // ShutdownInProgress
    133,   // FailedToSatisfyReadPreference
    150,   // StaleEpoch
    189,   // PrimarySteppedDown
    234,   // RetryChangeStream
    262,   // ExceededTimeLimit
    9001,  // SocketException
    10107, // NotWritablePrimary
    11600, // InterruptedAtShutdown
    11602, // InterruptedDueToReplStateChange
    13435, // NotPrimaryNoSecondaryOk
    13436, // NotPrimaryOrSecondary
];

/// Whether `code` is resumable for a change stream.
pub fn is_resumable_change_stream_code(code: i32) -> bool {
    RESUMABLE_CHANGE_STREAM_CODES.contains(&code)
}

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

#[cfg(test)]
mod resume_label_tests {
    use super::*;
    use bson::doc;

    /// `failGetMoreAfterCursorCheckout` is `failCommand` scoped to getMore, and
    /// mongod injects it inside the change-stream path — so the reply is
    /// marked server-injected and picks up the resumable label.
    #[test]
    fn get_more_after_cursor_checkout_is_scoped_and_server_injected() {
        let reg = FailPointRegistry::new();
        reg.configure(
            "failGetMoreAfterCursorCheckout",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"errorCode": 6_i32},
        );
        assert!(
            reg.match_command("find").is_none(),
            "scoped to getMore only"
        );

        let reg2 = FailPointRegistry::new();
        reg2.configure(
            "failGetMoreAfterCursorCheckout",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"errorCode": 6_i32},
        );
        let m = reg2.match_command("getMore").expect("getMore matches");
        assert_eq!(m.error_code, Some(6));
        assert!(m.server_injected, "must carry the resumable-label marker");
    }

    /// Plain `failCommand` is NOT server-injected: the change-streams spec
    /// requires `failCommand` + code 6 to surface the error rather than resume,
    /// precisely because no label is added.
    #[test]
    fn plain_fail_command_is_not_server_injected() {
        let reg = FailPointRegistry::new();
        reg.configure(
            "failCommand",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"failCommands": ["getMore"], "errorCode": 6_i32},
        );
        let m = reg.match_command("getMore").expect("matches");
        assert_eq!(m.error_code, Some(6));
        assert!(!m.server_injected, "no label ⇒ the driver must not resume");
    }

    /// The set mongod treats as resumable. `MaxTimeMSExpired` (50) is out: the
    /// spec resumes on it only when the failpoint sets the label explicitly.
    #[test]
    fn resumable_code_set_matches_the_spec() {
        for c in [
            6, 7, 63, 89, 91, 133, 150, 189, 234, 262, 9001, 10107, 11600, 11602, 13435, 13436,
        ] {
            assert!(is_resumable_change_stream_code(c), "{c} must be resumable");
        }
        for c in [50, 1, 11601, 280] {
            assert!(!is_resumable_change_stream_code(c), "{c} must not be");
        }
    }

    /// An unmodelled failpoint name stays accept-but-ignore.
    #[test]
    fn an_unknown_failpoint_name_is_still_ignored() {
        let reg = FailPointRegistry::new();
        reg.configure(
            "failAllRemoveOperations",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"errorCode": 6_i32},
        );
        assert!(reg.match_command("delete").is_none());
        assert!(reg.match_command("getMore").is_none());
    }
}

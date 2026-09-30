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
//! An optional `failCommands: [...]` filters by command name (empty == any),
//! compared against the CANONICAL name (`ismaster` counts as `isMaster`).
//!
//! `data.appName` scopes the failpoint to connections whose client metadata
//! names that application, as mongod does. Spec tests fail ONE client's
//! `hello` with `closeConnection` and rely on the runner's own client staying
//! usable; unscoped, that failpoint reached every connection -- including the
//! one that would switch it off -- and wedged the server.

use std::sync::Mutex;

use bson::{Bson, Document};

use crate::util::as_i64;

/// One configured `failCommand` failpoint.
struct FailCommand {
    fail_commands: Vec<String>,
    /// Fire only for connections whose client names this application.
    app_name: Option<String>,
    /// Configured as `failGetMoreAfterCursorCheckout` rather than
    /// `failCommand`. mongod injects that one *inside* the change-stream
    /// getMore path, where it stamps `ResumableChangeStreamError` on a
    /// resumable code; `failCommand` short-circuits earlier and carries only
    /// the labels the failpoint itself specified. The change-streams spec
    /// pins the difference: `failGetMoreAfterCursorCheckout` + code 6 resumes,
    /// `failCommand` + code 6 does not.
    server_injected: bool,
    error_code: Option<i32>,
    /// `None` when the failpoint did not mention `errorLabels` at all, which is
    /// NOT the same as `Some(vec![])`: mongod treats a supplied list as
    /// authoritative and adds nothing of its own to it, so an explicit `[]`
    /// means "no labels" rather than "you decide". Measured 2026-09-28 —
    /// injecting 11600 on `commitTransaction` answers `RetryableWriteError`
    /// with the key omitted and `[]` with it present. Collapsing the two with
    /// `unwrap_or_default` is what made the drivers' spec tests
    /// `commitTransaction does not retry error without RetryableWriteError
    /// label` (and its abort twin) fail.
    error_labels: Option<Vec<String>>,
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
    /// `None` == the failpoint said nothing about labels; see
    /// `FailCommand::error_labels`.
    pub error_labels: Option<Vec<String>>,
    pub write_concern_error: Option<Document>,
    pub close_connection: bool,
    pub block_time_ms: i64,
}

/// Thread-safe per-server registry of active failpoints.
#[derive(Default)]
pub struct FailPointRegistry {
    inner: Mutex<Vec<FailCommand>>,
    /// `maxTimeAlwaysTimeOut`: `None` when off, `Some(-1)` for `alwaysOn`, else
    /// the firings left under `{times: N}`.
    max_time_always: Mutex<Option<i64>>,
}

impl FailPointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install / replace / disable a named failpoint. `mode` is what mongod
    /// accepts (`"alwaysOn"` / `"off"` / `{times}` / `{skip, times}`).
    pub fn configure(&self, name: &str, mode: &Bson, data: &Document) {
        if name == "maxTimeAlwaysTimeOut" {
            self.configure_max_time_always(mode);
            return;
        }
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
        let app_name = data.get_str("appName").ok().map(String::from);
        let error_code = data.get("errorCode").and_then(as_i64).map(|n| n as i32);
        let server_injected = getmore_only;
        let error_labels = data.get_array("errorLabels").ok().map(|a| {
            a.iter()
                .filter_map(|b| b.as_str().map(String::from))
                .collect()
        });
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
            app_name,
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

    /// The decision for one incoming command `name` from a connection whose
    /// client names `app_name` (`None` when it sent none), consuming a
    /// `times`/`skip` budget only when the command is in scope -- another
    /// client's command must not use up a failpoint meant for this one. `None`
    /// means no failpoint applies.
    /// mongod's `maxTimeAlwaysTimeOut`: every operation that has a time limit
    /// expires at its first interrupt check, however large the budget. Mirrors
    /// `failpoints.py::_configure_max_time_always_timeout`.
    fn configure_max_time_always(&self, mode: &Bson) {
        let mut g = self
            .max_time_always
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *g = match mode {
            Bson::String(s) if s == "alwaysOn" => Some(-1),
            Bson::Document(m) => m.get("times").and_then(as_i64).filter(|n| *n > 0),
            _ => None,
        };
    }

    pub fn max_time_always_armed(&self) -> bool {
        self.max_time_always
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Fire once if armed, spending one of a `{times: N}` budget.
    pub fn consume_max_time_always(&self) -> bool {
        let mut g = self
            .max_time_always
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match *g {
            None => false,
            Some(n) if n > 0 => {
                *g = if n == 1 { None } else { Some(n - 1) };
                true
            }
            Some(_) => true,
        }
    }

    pub fn match_command(&self, name: &str, app_name: Option<&str>) -> Option<FailPointMatch> {
        let name = canonical_command_name(name);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for fc in g.iter_mut() {
            if !fc.fail_commands.is_empty() && !fc.fail_commands.iter().any(|c| c == name) {
                continue;
            }
            if fc.app_name.is_some() && fc.app_name.as_deref() != app_name {
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

/// The canonical name mongod registers a lower-case command alias under.
/// `failCommands` is compared against this, so a failpoint listing `isMaster`
/// also fires on the legacy `{ismaster: 1}` handshake -- which the SDAM spec
/// tests' `["hello", "isMaster"]` rely on.
fn canonical_command_name(name: &str) -> &str {
    match name {
        "ismaster" => "isMaster",
        "findandmodify" => "findAndModify",
        other => other,
    }
}

/// The application name a `failCommand` `appName` filter compares against.
/// A handshake `hello` carries `client.application.name` itself and that wins:
/// it is the FIRST command on a new connection, before any metadata is
/// recorded, and the SDAM spec tests fail exactly that command
/// (`minPoolSize-error.json`). Later commands use the recorded metadata.
pub fn failpoint_app_name(
    name: &str,
    doc: &Document,
    recorded: Option<&Document>,
) -> Option<String> {
    let in_flight = match name {
        "hello" | "isMaster" | "ismaster" => doc.get_document("client").ok(),
        _ => None,
    };
    in_flight
        .or(recorded)
        .and_then(|c| c.get_document("application").ok())
        .and_then(|a| a.get_str("name").ok())
        .map(String::from)
}

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

/// The codes mongod labels `RetryableWriteError` -- on a retryable write (a
/// write command carrying `txnNumber` outside a transaction) and on a failed
/// `commitTransaction` / `abortTransaction`, which are the same set.
///
/// Measured by sweeping `failCommand` over every code in 1..520 plus the
/// well-known high ones on a single-node replica-set mongod 8.2.11 (2026-09-30,
/// `tools/probes/error_labels.py`; commit measured over a raw socket). The
/// earlier list had 13 of these 23, taken from the codes a driver spec named;
/// and a retryable write was never labelled at all, so a driver saw the bare
/// code and did not retry.
///
/// 358 is here but not in [`TRANSIENT_TXN_CODES`]: a commit that fails with it
/// is retried, a statement that fails with it does not replay the transaction.
pub const RETRYABLE_WRITE_CODES: &[i32] = &[
    6, 7, 89, 91, 134, 189, 262, 317, 358, 384, 402, 406, 407, 412, 453, 462, 9001, 10107, 11600,
    11602, 13435, 13436, 50915,
];

/// Whether mongod labels `code` `RetryableWriteError` (see the constant).
pub fn is_retryable_write_code(code: i32) -> bool {
    RETRYABLE_WRITE_CODES.contains(&code)
}

/// The codes mongod labels `TransientTransactionError` on a STATEMENT inside a
/// transaction (`autocommit: false`), from the same sweep. On a commit or abort
/// the [`RETRYABLE_WRITE_CODES`] among them get `RetryableWriteError` instead
/// and the rest keep this label.
///
/// Deliberately absent, because mongod gives them no label there: 50
/// `MaxTimeMSExpired`, 100 `UnsatisfiableWriteConcern`, 11601 `Interrupted`,
/// and 11000 -- which aborts the transaction, but retrying would not help.
pub const TRANSIENT_TXN_CODES: &[i32] = &[
    6, 7, 24, 89, 91, 112, 134, 150, 189, 239, 246, 250, 251, 262, 267, 272, 317, 384, 402, 406,
    407, 412, 453, 462, 9001, 10107, 11600, 11602, 13435, 13436, 50915,
];

/// Whether mongod labels `code` `TransientTransactionError` in a transaction.
pub fn is_transient_txn_code(code: i32) -> bool {
    TRANSIENT_TXN_CODES.contains(&code)
}

/// The codes mongod labels `ResumableChangeStreamError` when `failCommand`
/// fails the AGGREGATE that opens a change stream (same sweep). Not its
/// `getMore`: there `failCommand` adds nothing, and the resumable label comes
/// only from `failGetMoreAfterCursorCheckout` -- [`RESUMABLE_CHANGE_STREAM_CODES`].
pub const CHANGE_STREAM_OPEN_RESUMABLE_CODES: &[i32] = &[
    6, 7, 89, 91, 133, 134, 150, 175, 189, 234, 262, 317, 358, 384, 401, 402, 406, 407, 412, 453,
    462, 9001, 10107, 11600, 11602, 13435, 13436, 50915,
];

/// Labelled `NonResumableChangeStreamError` on EVERY command, change stream or
/// not: 280 `ChangeStreamFatalError`, 286 `ChangeStreamHistoryLost`. The PHP
/// driver's `bug1419-001.phpt` fails a plain `find` cursor's getMore with 280.
pub const NON_RESUMABLE_CHANGE_STREAM_CODES: &[i32] = &[280, 286];

/// Labelled `SystemOverloadedError` on every command, after any other label.
pub const SYSTEM_OVERLOADED_CODES: &[i32] = &[433, 449, 450, 462];

/// The `codeName` mongod renders for a `failCommand`-injected code: its real
/// name where it has one (`mongod_codes`, measured), else `Location<code>`.
///
/// This used to be a hand list of 21 names read off a probe of the codes the
/// driver specs name. Every other code mongod names -- 400-odd, 391
/// `ReauthenticationRequired` among them -- came out as `Location<code>`.
pub fn fail_code_name(code: i32) -> String {
    match crate::mongod_codes::code_name(code) {
        Some(name) => name.to_string(),
        None => format!("Location{code}"),
    }
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
            reg.match_command("find", None).is_none(),
            "scoped to getMore only"
        );

        let reg2 = FailPointRegistry::new();
        reg2.configure(
            "failGetMoreAfterCursorCheckout",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"errorCode": 6_i32},
        );
        let m = reg2
            .match_command("getMore", None)
            .expect("getMore matches");
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
        let m = reg.match_command("getMore", None).expect("matches");
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
        assert!(reg.match_command("delete", None).is_none());
        assert!(reg.match_command("getMore", None).is_none());
    }

    /// `appName` scopes the failpoint to one client, and another client's
    /// commands neither fire it nor spend its `times` budget.
    #[test]
    fn app_name_scopes_to_one_client_without_spending_its_budget() {
        let reg = FailPointRegistry::new();
        reg.configure(
            "failCommand",
            &Bson::Document(doc! {"times": 1_i32}),
            &doc! {"failCommands": ["find"], "errorCode": 2_i32, "appName": "target"},
        );
        assert!(reg.match_command("find", None).is_none());
        assert!(reg.match_command("find", Some("other")).is_none());
        assert!(reg.match_command("find", Some("target")).is_some());
        assert!(
            reg.match_command("find", Some("target")).is_none(),
            "times: 1 is spent"
        );
    }

    /// `failCommands` compares canonical names: `isMaster` covers `ismaster`.
    #[test]
    fn fail_commands_match_the_canonical_name_of_an_alias() {
        let reg = FailPointRegistry::new();
        reg.configure(
            "failCommand",
            &Bson::String("alwaysOn".into()),
            &doc! {"failCommands": ["isMaster"], "errorCode": 2_i32},
        );
        assert!(reg.match_command("ismaster", None).is_some());
        assert!(reg.match_command("hello", None).is_none());
    }

    /// The handshake's own `client` document names the app before anything is
    /// recorded; later commands fall back to the recorded metadata.
    #[test]
    fn failpoint_app_name_prefers_the_handshake_client_document() {
        let hello = doc! {"hello": 1_i32, "client": {"application": {"name": "fresh"}}};
        let recorded = doc! {"application": {"name": "old"}};
        assert_eq!(
            failpoint_app_name("hello", &hello, Some(&recorded)).as_deref(),
            Some("fresh")
        );
        assert_eq!(
            failpoint_app_name("hello", &hello, None).as_deref(),
            Some("fresh")
        );
        let find = doc! {"find": "c", "client": {"application": {"name": "spoof"}}};
        assert_eq!(
            failpoint_app_name("find", &find, Some(&recorded)).as_deref(),
            Some("old")
        );
        assert_eq!(failpoint_app_name("find", &find, None), None);
    }

    /// Every row below was read off a single-node REPLICA SET mongod 8.2.11 on
    /// 2026-09-28, over a raw OP_MSG socket. Both qualifiers matter: transactions
    /// need a replica set, so the standalone used for most probing in this file
    /// cannot answer these at all; and a driver in the path is not safe here,
    /// because pymongo retries `commitTransaction` itself and converts the
    /// NotPrimary family into a client-side exception whose reply is never read
    /// — two earlier passes measured the driver and not the server.
    ///
    /// The table is the test. If mongod changes a name, this fails and someone
    /// re-probes; without it the only signal is a driver gauge going red for a
    /// reason nobody can localise.
    const MONGOD_8_2_11_FAIL_CODE_NAMES: &[(i32, &str)] = &[
        (6, "HostUnreachable"),
        (7, "HostNotFound"),
        (24, "LockTimeout"),
        (50, "MaxTimeMSExpired"),
        (89, "NetworkTimeout"),
        (91, "ShutdownInProgress"),
        (100, "UnsatisfiableWriteConcern"),
        (112, "WriteConflict"),
        (134, "ReadConcernMajorityNotAvailableYet"),
        (189, "PrimarySteppedDown"),
        (246, "SnapshotUnavailable"),
        (251, "NoSuchTransaction"),
        (262, "ExceededTimeLimit"),
        (267, "PreparedTransactionInProgress"),
        (9001, "SocketException"),
        (10107, "NotWritablePrimary"),
        (11600, "InterruptedAtShutdown"),
        (11601, "Interrupted"),
        (11602, "InterruptedDueToReplStateChange"),
        (13435, "NotPrimaryNoSecondaryOk"),
        (13436, "NotPrimaryOrSecondary"),
    ];

    #[test]
    fn fail_code_names_match_the_mongod_probe() {
        for (code, want) in MONGOD_8_2_11_FAIL_CODE_NAMES {
            assert_eq!(&fail_code_name(*code), want, "codeName for {code}");
        }
    }

    /// `Location<code>` is the fallback for a code mongod has no name for — not
    /// a licence to let a known name fall through it, which is how the drivers'
    /// `commitTransaction fails after Interrupted` spec asserted `Interrupted`
    /// and got `Location11601`.
    #[test]
    fn unknown_codes_still_fall_back_to_location() {
        assert_eq!(fail_code_name(987654), "Location987654");
        assert_ne!(fail_code_name(11601), "Location11601");
    }

    /// The commit/abort label split. The transaction-shaped failures (24, 112,
    /// 246, 251, 267) keep `TransientTransactionError` on commit; the
    /// reach-the-node failures get `RetryableWriteError`. The two sets are NOT
    /// complementary: 358 is retryable on a commit but not transient on a
    /// statement (measured 8.2.11, 2026-09-30).
    #[test]
    fn commit_retryable_and_transaction_transient_codes_are_disjoint() {
        for code in [24, 112, 246, 251, 267] {
            assert!(
                !is_retryable_write_code(code),
                "{code} is about the transaction, so commit keeps TransientTransactionError"
            );
        }
        for code in [
            6, 7, 89, 91, 134, 189, 262, 9001, 10107, 11600, 11602, 13435, 13436,
        ] {
            assert!(
                is_retryable_write_code(code),
                "{code} is a retryable-write failure, so commit gets RetryableWriteError"
            );
        }
        // Never labelled on commit at all — mongod gives these none.
        for code in [50, 100, 11601] {
            assert!(!is_retryable_write_code(code), "{code} earns no label");
        }
    }

    /// An `errorLabels` the failpoint actually supplied is authoritative, and an
    /// explicit `[]` is a supplied list. Collapsing the two is what made the
    /// specs' `commitTransaction does not retry error without RetryableWriteError
    /// label` fail: the server helpfully added a label the test required absent.
    #[test]
    fn explicit_error_labels_are_distinguished_from_an_absent_key() {
        let reg = FailPointRegistry::default();
        reg.configure(
            "failCommand",
            &Bson::String("alwaysOn".into()),
            &doc! {"failCommands": ["commitTransaction"], "errorCode": 11600_i32,
            "errorLabels": []},
        );
        let m = reg.match_command("commitTransaction", None).unwrap();
        assert_eq!(
            m.error_labels.as_deref(),
            Some(&[][..]),
            "an explicit [] must survive as Some(empty), not None"
        );

        let reg = FailPointRegistry::default();
        reg.configure(
            "failCommand",
            &Bson::String("alwaysOn".into()),
            &doc! {"failCommands": ["commitTransaction"], "errorCode": 11600_i32},
        );
        let m = reg.match_command("commitTransaction", None).unwrap();
        assert!(
            m.error_labels.is_none(),
            "an absent key must stay None so the server computes the label"
        );
    }
}

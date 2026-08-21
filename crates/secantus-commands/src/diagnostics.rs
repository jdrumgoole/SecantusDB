//! Session, transaction, and diagnostic commands — storage-light replies that
//! let drivers connect, attach sessions, tear down, and run admin probes
//! without hitting `CommandNotFound`.
//!
//! Ports of the small `commands.py` handlers. Since the Rust server doesn't yet
//! track a session/connection registry, the session commands are bookkeeping
//! no-ops (mongod returns `{ok:1}`) and `startSession` just mints an id. The
//! diagnostic commands return faithful-but-minimal shapes.
//!
//! **Deferred:** real session/cursor affinity; per-connection peer address for
//! `whatsmyuri`; live `connectionStatus` auth info (empty until R5 auth);
//! transaction semantics (commit/abort are accepted as no-ops, matching the
//! single-node surrogate).

use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};

use crate::util::command_error;
use crate::{CommandContext, HandlerResult, SERVER_VERSION};

/// `startSession` — mint a logical session id.
pub fn start_session(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    let bytes: [u8; 16] = rand::random();
    Ok(doc! {
        "id": { "id": Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes: bytes.to_vec() }) },
        "timeoutMinutes": 30_i32,
        "ok": 1.0,
    })
}

/// `endSessions` / `refreshSessions` / `killSessions` / `killAllSessions` /
/// `killAllSessionsByPattern` — no-op bookkeeping (no session registry yet).
pub fn ok_session_noop(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `commitTransaction` / `abortTransaction` — accepted as no-ops.
pub fn ok_transaction(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `whatsmyuri` — the client's connection peer (placeholder; peer tracking TBD).
pub fn whatsmyuri(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "you": "127.0.0.1:0", "ok": 1.0 })
}

/// `fsync` — flush data to disk. WiredTiger checkpoints on its own cadence, so
/// this reports success without forcing one. `lock: true` (which would block
/// writes until `fsyncUnlock` — needs coordination we don't have) is rejected
/// rather than silently skipped, so backup tools relying on the lock aren't
/// misled. Mirrors `commands.py::_fsync`.
pub fn fsync(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    if doc.get("lock").and_then(Bson::as_bool) == Some(true) {
        return Ok(crate::CommandError::new(
            9,
            "FailedToParse",
            "fsync with lock:true is not supported by SecantusDB",
        )
        .into_reply());
    }
    Ok(doc! { "numFiles": 1i32, "ok": 1.0 })
}

/// `killOp` — close a client connection by its `op` (our per-connection
/// `conn_id`; we model one in-flight op per connection, so the opid a caller
/// reads off `hello`'s `connectionId` / `currentOp` is directly killable). Real
/// mongod signals a per-op interrupt flag long-running paths poll; SecantusDB's
/// faithful analogue is "close the socket" — the connection thread's next read
/// returns 0, the loop exits, and the connection unregisters. The opid is
/// accepted as Int32 / Int64 / an integral Double / a numeric string (drivers
/// serialise it differently). Mirrors `commands.py::_kill_op`.
pub fn kill_op(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let op_id: i64 = match doc.get("op") {
        Some(Bson::Int32(n)) => i64::from(*n),
        Some(Bson::Int64(n)) => *n,
        Some(Bson::Double(d)) if d.fract() == 0.0 => *d as i64,
        Some(Bson::String(s)) if s.parse::<i64>().is_ok() => s.parse::<i64>().unwrap(),
        other => {
            return Err(crate::CommandError::new(
                14,
                "TypeMismatch",
                format!("killOp requires an integer `op` field, got {other:?}"),
            ));
        }
    };
    let info = match &ctx.conn_killer {
        None => "no connection registry",
        Some(killer) if killer.kill(op_id) => "operation killed",
        Some(_) => "no operation with that opid",
    };
    Ok(doc! { "info": info, "ok": 1.0 })
}

/// `connectionStatus` — auth info for the connection (empty until R5 auth).
pub fn connection_status(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! {
        "authInfo": {
            "authenticatedUsers": Vec::<Bson>::new(),
            "authenticatedUserRoles": Vec::<Bson>::new(),
            "authenticatedUserPrivileges": Vec::<Bson>::new(),
        },
        "ok": 1.0,
    })
}

/// `getCmdLineOpts` — parsed command line; drivers read
/// `parsed.security.authorization` to detect `--auth`.
pub fn get_cmd_line_opts(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let mut parsed = doc! { "net": {}, "storage": {} };
    let mut argv = vec![Bson::String("secantus".to_string())];
    if ctx.require_auth {
        parsed.insert("security", doc! { "authorization": "enabled" });
        argv.push(Bson::String("--auth".to_string()));
    }
    Ok(doc! { "argv": argv, "parsed": parsed, "ok": 1.0 })
}

/// `hostInfo` — host / OS / CPU info (minimal).
pub fn host_info(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1);
    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "secantus".to_string());
    Ok(doc! {
        "system": {
            "hostname": hostname,
            "cpuArch": std::env::consts::ARCH,
            "numCores": cores,
            "memSizeMB": 0_i64,
        },
        "os": { "type": std::env::consts::OS, "name": std::env::consts::OS, "version": "" },
        "extra": {},
        "ok": 1.0,
    })
}

/// `getLog` — the server's in-memory log ring buffer. mongod returns the log as
/// a list of pre-formatted strings (`"<ts> <level> <component> <msg>"`); we
/// mirror that format so tooling that grep-parses the response keeps working.
/// Without a buffer wired in (unit tests), reports an empty log. Mirrors
/// `commands.py::_get_log`.
pub fn get_log(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let Some(logs) = &ctx.logs else {
        return Ok(doc! { "totalLinesWritten": 0_i32, "log": Vec::<Bson>::new(), "ok": 1.0 });
    };
    let entries = logs.tail(None);
    let formatted: Vec<Bson> = entries
        .iter()
        .map(|e| {
            let ts = e.ts.try_to_rfc3339_string().unwrap_or_default();
            Bson::String(format!("{ts} {} {} {}", e.level, e.component, e.msg))
        })
        .collect();
    Ok(doc! {
        "totalLinesWritten": entries.len() as i32,
        "log": formatted,
        "ok": 1.0,
    })
}

/// `getParameter` — a minimal set of well-known server parameters.
pub fn get_parameter(doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    let params = known_params();
    let arg = doc.get("getParameter");
    // "*" or the legacy `{getParameter: 1}` with no names ⇒ all params.
    let names: Vec<&String> = doc
        .keys()
        .filter(|k| *k != "getParameter" && !k.starts_with('$'))
        .collect();
    let want_all = matches!(arg, Some(Bson::String(s)) if s == "*") || names.is_empty();
    let mut out = Document::new();
    if want_all {
        for (k, v) in params.iter() {
            out.insert(k.clone(), v.clone());
        }
    } else {
        for n in names {
            if let Some(v) = params.get(n) {
                out.insert(n.clone(), v.clone());
            }
        }
    }
    out.insert("ok", 1.0);
    Ok(out)
}

fn known_params() -> Document {
    doc! {
        "featureCompatibilityVersion": { "version": "7.0" },
        "enableTestCommands": false,
        "logLevel": 0_i32,
        "quiet": false,
        // We implement SCRAM-SHA-256 + MONGODB-X509 (R5); advertise just those
        // so driver test runners gating on other mechanisms self-skip.
        "authenticationMechanisms": [
            Bson::String("SCRAM-SHA-256".to_string()),
            Bson::String("MONGODB-X509".to_string()),
        ],
        "version": SERVER_VERSION,
    }
}

/// `top` — per-namespace operation counters, mongod-shaped.
///
/// SecantusDB does not instrument per-namespace operation timing, so every
/// counter is zero and `mongotop` renders the all-zero table it shows for an
/// idle mongod. The shape is what mongo-tools' decoder requires: a `note` key it
/// skips explicitly, then one entry per namespace holding
/// `total`/`readLock`/`writeLock` plus the per-op sections, each `{time, count}`.
///
/// Ported from `commands.py::_top`, which shipped first. Until this landed the
/// Rust server answered `top` with CommandNotFound (59), so `mongotop` failed
/// outright against it rather than rendering an idle server — a gap the Python
/// entry's "counters are always zero" wording hid.
pub fn top(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.db_name != "admin" {
        return Ok(doc! {
            "ok": 0.0,
            "errmsg": "top may only be run against the admin database.",
            "code": 13_i32,
            "codeName": "Unauthorized",
        });
    }
    const SECTIONS: [&str; 8] = [
        "total",
        "readLock",
        "writeLock",
        "queries",
        "getmore",
        "insert",
        "update",
        "remove",
    ];
    let storage = ctx.storage()?;
    let mut totals = doc! { "note": "all times in microseconds" };
    for db in storage.list_databases().map_err(command_error)? {
        for coll in storage.list_collections(&db).map_err(command_error)? {
            let mut ns = Document::new();
            for section in SECTIONS {
                ns.insert(section, doc! { "time": 0_i64, "count": 0_i64 });
            }
            // `commands` is a section too; kept out of the array above only
            // because the name would read oddly beside the per-op verbs.
            ns.insert("commands", doc! { "time": 0_i64, "count": 0_i64 });
            totals.insert(format!("{db}.{coll}"), ns);
        }
    }
    Ok(doc! { "totals": totals, "ok": 1.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;

    fn ctx() -> CommandContext {
        CommandContext::new(1)
    }

    #[test]
    fn start_session_mints_uuid() {
        let reply = dispatch(&doc! {"startSession": 1}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        let id = reply.get_document("id").unwrap();
        match id.get("id") {
            Some(Bson::Binary(b)) => {
                assert_eq!(b.subtype, BinarySubtype::Uuid);
                assert_eq!(b.bytes.len(), 16);
            }
            other => panic!("expected UUID binary, got {other:?}"),
        }
    }

    /// A `ConnectionKiller` that records the ids it was asked to kill and reports
    /// a fixed found/not-found result.
    struct FakeKiller {
        found: bool,
        killed: std::sync::Mutex<Vec<i64>>,
    }
    impl crate::ConnectionKiller for FakeKiller {
        fn kill(&self, conn_id: i64) -> bool {
            self.killed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(conn_id);
            self.found
        }
    }

    #[test]
    fn kill_op_without_registry_reports_no_registry() {
        let reply = dispatch(&doc! {"killOp": 1, "op": 7_i32}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
        assert_eq!(reply.get_str("info").unwrap(), "no connection registry");
    }

    #[test]
    fn kill_op_kills_a_known_connection() {
        let killer = std::sync::Arc::new(FakeKiller {
            found: true,
            killed: Default::default(),
        });
        let mut c = ctx().with_conn_killer(killer.clone());
        let reply = dispatch(&doc! {"killOp": 1, "op": 42_i64}, &mut c);
        assert_eq!(reply.get_str("info").unwrap(), "operation killed");
        assert_eq!(
            *killer.killed.lock().unwrap_or_else(|e| e.into_inner()),
            vec![42]
        );
    }

    #[test]
    fn kill_op_unknown_opid_reports_no_operation() {
        let killer = std::sync::Arc::new(FakeKiller {
            found: false,
            killed: Default::default(),
        });
        let mut c = ctx().with_conn_killer(killer);
        let reply = dispatch(&doc! {"killOp": 1, "op": 99_i32}, &mut c);
        assert_eq!(
            reply.get_str("info").unwrap(),
            "no operation with that opid"
        );
    }

    #[test]
    fn kill_op_accepts_a_numeric_string_op() {
        let killer = std::sync::Arc::new(FakeKiller {
            found: true,
            killed: Default::default(),
        });
        let mut c = ctx().with_conn_killer(killer.clone());
        let reply = dispatch(&doc! {"killOp": 1, "op": "7"}, &mut c);
        assert_eq!(reply.get_str("info").unwrap(), "operation killed");
        assert_eq!(
            *killer.killed.lock().unwrap_or_else(|e| e.into_inner()),
            vec![7]
        );
    }

    #[test]
    fn kill_op_non_integer_op_is_type_mismatch() {
        let reply = dispatch(&doc! {"killOp": 1, "op": "not-an-int"}, &mut ctx());
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 14);
        assert_eq!(reply.get_str("codeName").unwrap(), "TypeMismatch");
    }

    #[test]
    fn get_log_without_buffer_is_empty() {
        let reply = dispatch(&doc! {"getLog": "global"}, &mut ctx());
        assert_eq!(reply.get_i32("totalLinesWritten").unwrap(), 0);
        assert_eq!(reply.get_array("log").unwrap().len(), 0);
    }

    #[test]
    fn get_log_returns_buffered_lines_formatted() {
        let logs = std::sync::Arc::new(crate::logbuf::LogBuffer::new());
        logs.append("I", "NETWORK", "connection accepted #1");
        logs.append("I", "CONTROL", "started");
        let mut c = ctx().with_logs(logs);
        let reply = dispatch(&doc! {"getLog": "global"}, &mut c);
        assert_eq!(reply.get_i32("totalLinesWritten").unwrap(), 2);
        let log = reply.get_array("log").unwrap();
        assert_eq!(log.len(), 2);
        // Format is "<ts> <level> <component> <msg>".
        let line0 = log[0].as_str().unwrap();
        assert!(
            line0.contains("I NETWORK connection accepted #1"),
            "unexpected log line: {line0}"
        );
    }

    #[test]
    fn session_and_txn_noops_ok() {
        for cmd in [
            doc! {"endSessions": []},
            doc! {"refreshSessions": []},
            doc! {"killSessions": []},
            doc! {"killAllSessions": []},
            doc! {"commitTransaction": 1},
            doc! {"abortTransaction": 1},
        ] {
            assert_eq!(dispatch(&cmd, &mut ctx()).get_f64("ok").unwrap(), 1.0);
        }
    }

    #[test]
    fn get_parameter_named_and_all() {
        // named subset
        let reply = dispatch(
            &doc! {"getParameter": 1, "featureCompatibilityVersion": 1},
            &mut ctx(),
        );
        assert!(reply.get_document("featureCompatibilityVersion").is_ok());
        assert!(
            reply.get("logLevel").is_none(),
            "only requested param returned"
        );
        // all
        let all = dispatch(&doc! {"getParameter": "*"}, &mut ctx());
        assert!(all.get("authenticationMechanisms").is_some());
        assert!(all.get("logLevel").is_some());
    }

    #[test]
    fn get_cmd_line_opts_reflects_auth() {
        // Call the handler directly: under `--auth`, dispatch would gate
        // `getCmdLineOpts` behind authentication (covered in the auth tests),
        // so here we verify only that the handler reflects `--auth` in its
        // `parsed.security` output.
        let mut c = ctx();
        let off = get_cmd_line_opts(&doc! {"getCmdLineOpts": 1}, &mut c).unwrap();
        assert!(off
            .get_document("parsed")
            .unwrap()
            .get("security")
            .is_none());
        c.require_auth = true;
        let on = get_cmd_line_opts(&doc! {"getCmdLineOpts": 1}, &mut c).unwrap();
        assert_eq!(
            on.get_document("parsed")
                .unwrap()
                .get_document("security")
                .unwrap()
                .get_str("authorization")
                .unwrap(),
            "enabled"
        );
    }

    #[test]
    fn connection_status_and_diagnostics_shapes() {
        assert!(dispatch(&doc! {"connectionStatus": 1}, &mut ctx())
            .get_document("authInfo")
            .is_ok());
        assert_eq!(
            dispatch(&doc! {"whatsmyuri": 1}, &mut ctx())
                .get_f64("ok")
                .unwrap(),
            1.0
        );
        assert!(dispatch(&doc! {"hostInfo": 1}, &mut ctx())
            .get_document("system")
            .is_ok());

        // `top` outside the admin database is refused exactly as commands.py
        // refuses it. The Rust server answered CommandNotFound before this
        // landed, so mongotop failed outright rather than showing an idle table.
        // (The test context defaults to `admin`, so name a user db explicitly.)
        let mut user_db = ctx();
        user_db.db_name = "shop".to_string();
        let refused = dispatch(&doc! {"top": 1}, &mut user_db);
        assert_eq!(refused.get_f64("ok").unwrap(), 0.0);
        assert_eq!(refused.get_i32("code").unwrap(), 13);
        assert_eq!(
            dispatch(&doc! {"getLog": "global"}, &mut ctx())
                .get_i32("totalLinesWritten")
                .unwrap(),
            0
        );
    }
}

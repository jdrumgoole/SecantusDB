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
use crate::{CommandContext, HandlerResult};

/// `startSession` — mint a logical session id.
pub fn start_session(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    let bytes: [u8; 16] = rand::random();
    Ok(doc! {
        "id": { "id": Bson::Binary(Binary { subtype: BinarySubtype::Uuid, bytes: bytes.to_vec() }) },
        "timeoutMinutes": 30_i32,
        "ok": 1.0,
    })
}

/// `refreshSessions` — no-op bookkeeping (no session registry yet). The
/// session-ENDING commands live in `lib.rs`: they abort open transactions.
pub fn ok_session_noop(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `commitTransaction` / `abortTransaction` — accepted as no-ops.
pub fn ok_transaction(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "ok": 1.0 })
}

/// `fsync` — flush data to disk: a checkpoint, as mongod takes one, so what a
/// background `validate` reads afterwards is the store as it is now.
/// `lock: true` then holds writes off until as many `fsyncUnlock`s
/// (`LockManager::fsync_lock`): what backup tools pair it with. Measured
/// 8.2.11, 2026-10-20; this refused the lock.
pub fn fsync(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let lock = match doc.get("lock") {
        Some(Bson::Boolean(b)) => *b,
        Some(Bson::Int32(n)) => *n != 0,
        Some(Bson::Int64(n)) => *n != 0,
        Some(Bson::Double(d)) => *d != 0.0,
        _ => false,
    };
    ctx.storage()?
        .checkpoint_now()
        .map_err(crate::util::command_error)?;
    if !lock {
        return Ok(doc! { "numFiles": 1i32, "ok": 1.0 });
    }
    let Some(locks) = ctx.locks.clone() else {
        return Err(crate::CommandError::new(
            8,
            "UnknownError",
            "fsync lock needs the server's lock manager",
        ));
    };
    let count = locks.fsync_lock();
    Ok(doc! {
        "info": "now locked against writes, use db.fsyncUnlock() to unlock",
        "lockCount": count as i32,
        "seeAlso": "http://dochub.mongodb.org/core/fsynccommand",
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

/// `hostInfo`: the machine's own figures, in mongod's keys, types and order
/// (measured 8.2.11 on macOS arm64, 2026-10-15). `hostname` is the host's
/// name and the server's port, as mongod gives it. A figure this platform
/// cannot read is left out rather than reported as zero.
pub fn host_info(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let host = crate::hostinfo::Host::read();
    let hostname = match &ctx.server_address {
        Some((_, port)) => format!("{}:{port}", host.hostname),
        None => host.hostname.clone(),
    };
    let mut system = doc! {
        "currentTime": bson::DateTime::now(),
        "hostname": hostname,
        "cpuAddrSize": (usize::BITS) as i32,
    };
    if let Some(mb) = host.mem_size_mb {
        system.insert("memSizeMB", mb);
        system.insert("memLimitMB", host.mem_limit_mb.unwrap_or(mb));
    }
    system.insert("numCores", host.logical_cores);
    system.insert("numCoresAvailableToProcess", host.available_cores);
    if let Some(n) = host.physical_cores {
        system.insert("numPhysicalCores", n);
    }
    if let Some(n) = host.sockets {
        system.insert("numCpuSockets", n);
    }
    system.insert("cpuArch", host.machine.clone());
    system.insert("numaEnabled", false);
    system.insert("numNumaNodes", 1_i32);
    Ok(doc! {
        "system": system,
        "os": {"type": host.os_type, "name": host.os_name, "version": host.os_version},
        "extra": host.extra,
        "ok": 1.0,
    })
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
    let stats = ctx.top_stats.clone();
    let storage = ctx.storage()?;
    let mut totals = doc! { "note": "all times in microseconds" };
    for db in storage.list_databases().map_err(command_error)? {
        for coll in storage.list_collections(&db).map_err(command_error)? {
            let name = format!("{db}.{coll}");
            let recorded = stats.as_ref().and_then(|s| s.snapshot_ns(&name));
            let mut ns = Document::new();
            for section in crate::topstats::TOP_SECTIONS {
                let slot = recorded.as_ref().and_then(|r| r.get(section)).copied();
                let slot = slot.unwrap_or_default();
                ns.insert(section, doc! { "time": slot.micros, "count": slot.count });
            }
            totals.insert(name, ns);
        }
    }
    Ok(doc! { "totals": totals, "ok": 1.0 })
}

#[cfg(test)]
mod top_counter_tests {
    use super::*;
    use crate::storage::{Storage, StorageError};
    use crate::topstats::TopStats;
    use std::sync::Arc;

    /// Minimal catalog stub: `top` only walks databases and collections, so
    /// the data-bearing methods are unreachable here.
    struct OneCollection;
    impl Storage for OneCollection {
        fn list_databases(&self) -> Result<Vec<String>, StorageError> {
            Ok(vec!["db".to_string()])
        }
        fn list_collections(&self, _db: &str) -> Result<Vec<String>, StorageError> {
            Ok(vec!["c".to_string()])
        }
        fn insert(
            &self,
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), StorageError> {
            unreachable!("top does not insert")
        }
        fn update_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<crate::storage::UpdateOutcome, StorageError> {
            unreachable!("top does not update")
        }
        fn delete_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, StorageError> {
            unreachable!("top does not delete")
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, StorageError> {
            unreachable!("top does not count")
        }
        fn find(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _sort: Option<&Document>,
            _hint: Option<crate::storage::RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, StorageError> {
            unreachable!("top does not find")
        }
    }

    fn run_top(stats: Option<Arc<TopStats>>) -> Document {
        let mut ctx = CommandContext::new(1).with_storage(Arc::new(OneCollection));
        ctx.db_name = "admin".to_string();
        if let Some(s) = stats {
            ctx = ctx.with_top_stats(s);
        }
        let reply = top(&doc! {"top": 1}, &mut ctx).unwrap();
        reply.get_document("totals").unwrap().clone()
    }

    #[test]
    fn recorded_work_reaches_the_reply() {
        let stats = Arc::new(TopStats::new());
        stats.record("db.c", "insert", 100);
        stats.record("db.c", "insert", 50);
        stats.record("db.c", "find", 20);

        let ns = run_top(Some(stats)).get_document("db.c").unwrap().clone();
        assert_eq!(
            ns.get_document("insert").unwrap().get_i64("count").unwrap(),
            2
        );
        assert_eq!(
            ns.get_document("insert").unwrap().get_i64("time").unwrap(),
            150
        );
        assert_eq!(
            ns.get_document("queries")
                .unwrap()
                .get_i64("count")
                .unwrap(),
            1
        );
        assert_eq!(
            ns.get_document("total").unwrap().get_i64("count").unwrap(),
            3
        );
        assert_eq!(
            ns.get_document("total").unwrap().get_i64("time").unwrap(),
            170
        );
        // total == readLock + writeLock, as on mongod.
        assert_eq!(
            ns.get_document("readLock")
                .unwrap()
                .get_i64("count")
                .unwrap(),
            1
        );
        assert_eq!(
            ns.get_document("writeLock")
                .unwrap()
                .get_i64("count")
                .unwrap(),
            2
        );
    }

    #[test]
    fn shape_is_unchanged_and_complete() {
        let totals = run_top(Some(Arc::new(TopStats::new())));
        assert_eq!(totals.get_str("note").unwrap(), "all times in microseconds");
        let ns = totals.get_document("db.c").unwrap();
        for section in crate::topstats::TOP_SECTIONS {
            let sec = ns.get_document(section).unwrap();
            assert!(sec.get_i64("time").is_ok(), "{section} missing time");
            assert!(sec.get_i64("count").is_ok(), "{section} missing count");
        }
        assert_eq!(ns.len(), crate::topstats::TOP_SECTIONS.len());
    }

    #[test]
    fn without_stats_every_counter_is_zero() {
        // Off-server (unit-test) contexts have no counters wired; `top` must
        // still answer with the full mongod shape rather than erroring.
        let ns = run_top(None).get_document("db.c").unwrap().clone();
        for section in crate::topstats::TOP_SECTIONS {
            assert_eq!(
                ns.get_document(section).unwrap().get_i64("count").unwrap(),
                0
            );
        }
    }

    #[test]
    fn non_admin_db_is_still_refused() {
        let mut ctx = CommandContext::new(1).with_storage(Arc::new(OneCollection));
        ctx.db_name = "notadmin".to_string();
        let reply = top(&doc! {"top": 1}, &mut ctx).unwrap();
        assert_eq!(reply.get_f64("ok").unwrap(), 0.0);
        assert_eq!(reply.get_i32("code").unwrap(), 13);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use crate::SERVER_VERSION_ARRAY;

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
        assert_eq!(reply.get_str("info").unwrap(), "attempting to kill op");
    }

    /// `killOp` marks the operation its opid names killed, and closes no
    /// connection: it used to close the connection whose id matched the
    /// opid (RM-216).
    #[test]
    fn kill_op_marks_a_running_operation_and_closes_no_connection() {
        let killer = std::sync::Arc::new(FakeKiller {
            found: true,
            killed: Default::default(),
        });
        let ops = std::sync::Arc::new(crate::ops::OpRegistry::new());
        let mut c = ctx().with_conn_killer(killer.clone()).with_ops(ops.clone());
        let running = crate::ops::begin(&ops, &doc! {"getMore": 5_i64, "collection": "c"}, &ctx());
        let opid = ops.snapshot()[0].opid;
        let reply = dispatch(&doc! {"killOp": 1, "op": opid}, &mut c);
        assert_eq!(reply.get_str("info").unwrap(), "attempting to kill op");
        assert!(ops
            .snapshot()
            .iter()
            .any(|op| op.opid == opid && op.killed()));
        assert!(killer
            .killed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
        drop(running);
        assert!(ops.snapshot().is_empty());
        // An opid that names nothing running gets the same answer.
        let reply = dispatch(&doc! {"killOp": 1, "op": 99_i32}, &mut c);
        assert_eq!(reply.get_str("info").unwrap(), "attempting to kill op");
    }

    /// mongod takes a number and nothing else: a string of digits is a
    /// type error, and nothing is closed for it.
    #[test]
    fn kill_op_refuses_a_numeric_string_op() {
        let killer = std::sync::Arc::new(FakeKiller {
            found: true,
            killed: Default::default(),
        });
        let mut c = ctx().with_conn_killer(killer.clone());
        let reply = dispatch(&doc! {"killOp": 1, "op": "7"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 14);
        assert!(killer
            .killed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
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
        logs.log(crate::logbuf::LogEntry::new(
            "I",
            "NETWORK",
            22943,
            "listener",
            "Connection accepted",
        ));
        logs.log(crate::logbuf::LogEntry::new(
            "I",
            "CONTROL",
            4615611,
            "initandlisten",
            "MongoDB starting",
        ));
        let mut c = ctx().with_logs(logs);
        let reply = dispatch(&doc! {"getLog": "global"}, &mut c);
        assert_eq!(reply.get_i32("totalLinesWritten").unwrap(), 2);
        let log = reply.get_array("log").unwrap();
        assert_eq!(log.len(), 2);
        // mongod's structured log: one JSON document per line.
        let line0 = log[0].as_str().unwrap();
        assert!(
            line0.contains(
                r#""c":"NETWORK",  "id":22943,   "ctx":"listener","msg":"Connection accepted"}"#
            ),
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
        ] {
            assert_eq!(dispatch(&cmd, &mut ctx()).get_f64("ok").unwrap(), 1.0);
        }
        // Ending a transaction needs one: 125, as mongod answers.
        for cmd in [doc! {"commitTransaction": 1}, doc! {"abortTransaction": 1}] {
            assert_eq!(dispatch(&cmd, &mut ctx()).get_i32("code").unwrap(), 125);
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
        // Read the VALUE, not just the key. Asserting presence alone is what
        // let this sit at "7.0" through the retarget to 8.x: mongod 8.2.11
        // answers major.minor of the running binary, so it must track
        // SERVER_VERSION_ARRAY and never be a second literal.
        assert_eq!(
            reply
                .get_document("featureCompatibilityVersion")
                .unwrap()
                .get_str("version")
                .unwrap(),
            format!("{}.{}", SERVER_VERSION_ARRAY[0], SERVER_VERSION_ARRAY[1]),
            "FCV must be major.minor of the advertised server version"
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

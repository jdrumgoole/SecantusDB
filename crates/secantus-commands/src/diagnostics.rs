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

/// `getLog` — the in-memory log buffer (empty; no log buffer yet).
pub fn get_log(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(doc! { "totalLinesWritten": 0_i32, "log": Vec::<Bson>::new(), "ok": 1.0 })
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
        assert_eq!(
            dispatch(&doc! {"getLog": "global"}, &mut ctx())
                .get_i32("totalLinesWritten")
                .unwrap(),
            0
        );
    }
}

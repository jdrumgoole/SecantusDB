//! What an interrupted operation answers (RM-313).
//!
//! An operation is interrupted by `killOp`, by a kill of the session it runs
//! on (`killSessions`, `killAllSessions`, `killAllSessionsByPattern`) or by
//! its spent `maxTimeMS`. Each interrupt point polls
//! [`secantus_core::deadline`] -- a scan's documents, an expression's
//! elements, a write batch's statements -- and fails with the budget's error;
//! the operation's kill flag says which of the two it was. This module turns
//! that failure into mongod 8.2.11's reply, measured 2026-10-27 against a
//! replica set and a standalone:
//!
//! * `update` / `delete` (and `bulkWrite`): the whole command fails,
//!   `ok: 0`, under `Plan executor error during <command> :: caused by ::`
//!   when the interrupt landed in a statement's plan and bare between
//!   statements, with no `n`; the statements before stay applied.
//! * `insert`: the bare message.
//! * `findAndModify`: the bare message on a replica set, and under
//!   `Plan executor error during findAndModify :: caused by ::` on a
//!   standalone.
//! * Outside a transaction, on a replica set, an insert, update or delete
//!   that waits for its write concern finds the wait interrupted too, and
//!   carries a `writeConcernError` with the same code -- on an `ok: 1` reply
//!   as well, when the kill landed after the write was done. A write concern
//!   of `{w: 1}` waits for nothing, and carries none.
//! * A read's error goes under its executor wrapper.

use bson::{doc, Bson, Document};

use crate::CommandContext;

/// mongod's error for an interrupted operation: 11601 for a kill, 50 for a
/// spent budget.
fn error_of(killed: bool) -> (i32, &'static str, &'static str) {
    if killed {
        (11601, "Interrupted", "operation was interrupted")
    } else {
        use secantus_core::deadline::MaxTimeMsExpired as E;
        (E::CODE, E::CODE_NAME, E::MESSAGE)
    }
}

fn is_write(name: &str) -> bool {
    matches!(name, "insert" | "update" | "delete" | "bulkWrite")
}

fn is_find_and_modify(name: &str) -> bool {
    matches!(name, "findAndModify" | "findandmodify")
}

fn ok(reply: &Document) -> bool {
    reply.get_f64("ok").unwrap_or(0.0) == 1.0
}

/// Whether a write statement failed with the budget's error: how an
/// interrupted statement is reported by the write handlers, so that the
/// statements before it count as applied (and, for a retryable write, as
/// done).
fn interrupted_statement(reply: &Document) -> Option<&str> {
    // A `bulkWrite` reports its ops' failures in its cursor.
    let failures = match reply.get_document("cursor") {
        Ok(cursor) => cursor.get_array("firstBatch").ok()?,
        Err(_) => reply.get_array("writeErrors").ok()?,
    };
    failures.iter().find_map(|e| {
        let e = e.as_document()?;
        let interrupted = e.get("code").and_then(crate::util::as_i64)
            == Some(i64::from(secantus_core::deadline::MaxTimeMsExpired::CODE));
        interrupted.then(|| e.get_str("errmsg").unwrap_or_default())
    })
}

/// Whether a write sent with this write concern waits after it is done --
/// for other members or for the journal -- and so finds a kill there. The
/// implicit default is `{w: "majority"}`. Measured 8.2.11: `{w: 1}`,
/// `{w: 1, wtimeout: 100}` and `{w: 1, j: false}` carry no
/// `writeConcernError` when killed; `w: "majority"`, `{w: 1, j: true}`,
/// `{w: 1, fsync: true}` and `{j: false}` (the default `w` still applies)
/// do.
fn waits_for_write_concern(client: Option<&Document>, default: Option<&Document>) -> bool {
    let client = client.filter(|wc| !wc.is_empty());
    let truthy = |wc: Option<&Document>, key: &str| {
        wc.and_then(|wc| wc.get(key)).is_some_and(|v| match v {
            Bson::Boolean(b) => *b,
            other => crate::util::as_i64(other).is_some_and(|n| n != 0),
        })
    };
    if truthy(client, "j") || truthy(client, "fsync") {
        return true;
    }
    let w = client
        .and_then(|wc| wc.get("w"))
        .or_else(|| default.and_then(|wc| wc.get("w")));
    match w {
        // The implicit default.
        None => true,
        Some(Bson::Int32(n)) => *n > 1,
        Some(Bson::Int64(n)) => *n > 1,
        Some(Bson::Double(d)) => *d > 1.0,
        Some(w @ Bson::Decimal128(_)) => crate::util::as_i64(w).is_none_or(|n| n > 1),
        // `majority` or a tag.
        Some(_) => true,
    }
}

/// The `writeConcernError` of a write whose write-concern wait was
/// interrupted. mongod names `{w: 1, wtimeout: 0}` in it whatever was asked
/// (measured 8.2.11 for `majority`, `{w: 1, j: true}` and the default).
fn write_concern_error(killed: bool) -> Document {
    let (code, code_name, message) = error_of(killed);
    doc! {
        "code": code,
        "codeName": code_name,
        "errmsg": message,
        "errInfo": {"writeConcern": {"w": 1_i32, "wtimeout": 0_i32}},
    }
}

/// Rewrite `reply` for an interrupted operation, as mongod answers it.
/// `client_wc` / `default_wc` are the write concern the command was sent
/// with and the stored default it falls back to.
pub(crate) fn finish(
    name: &str,
    doc: &Document,
    reply: &mut Document,
    ctx: &CommandContext,
    client_wc: Option<&Document>,
    default_wc: Option<&Document>,
) {
    let killed = crate::ops::current_killed();
    let tripped = secantus_core::deadline::tripped();
    if !killed && !tripped {
        return;
    }
    let in_transaction = doc.contains_key("autocommit");
    let replica_set = ctx.replica_set_name.is_some();
    let unacknowledged = client_wc
        .and_then(|wc| wc.get("w"))
        .and_then(crate::util::as_i64)
        == Some(0);
    // `createIndexes` waits for its write concern too (measured killed in a
    // lock wait); `drop` answered none.
    let wce = (is_write(name) || name == "createIndexes")
        && !in_transaction
        && replica_set
        && !unacknowledged
        && waits_for_write_concern(client_wc, default_wc);
    let code = reply.get_i32("code").ok();
    let budget_code = secantus_core::deadline::MaxTimeMsExpired::CODE;
    if is_write(name) || is_find_and_modify(name) {
        // The interrupted statement's message, or the command's.
        let failed = if ok(reply) {
            interrupted_statement(reply)
        } else if code == Some(budget_code) {
            Some(reply.get_str("errmsg").unwrap_or_default())
        } else {
            None
        }
        .map(str::to_string);
        // A budget spent away from a poll and the kill (a lock wait's)
        // keeps the statement's own answer.
        if let (true, Some(source)) = (tripped || killed, failed) {
            let (code, code_name, message) = error_of(killed);
            // An update's or a delete's statement says whether its plan was
            // running (the executor's wrapper) or not (bare), and so does a
            // `bulkWrite`'s op. mongod sends `findAndModify`'s bare on a
            // replica set and wrapped on a standalone, and an insert's bare.
            let errmsg = match name {
                "findAndModify" | "findandmodify" if tripped && !replica_set => {
                    format!("Plan executor error during findAndModify :: caused by :: {message}")
                }
                "update" | "delete" | "bulkWrite" => {
                    source.replace(secantus_core::deadline::MaxTimeMsExpired::MESSAGE, message)
                }
                _ => message.to_string(),
            };
            let mut out = Document::new();
            if wce {
                out.insert("writeConcernError", write_concern_error(killed));
            }
            out.insert("ok", 0.0);
            out.insert("errmsg", errmsg);
            out.insert("code", code);
            out.insert("codeName", code_name);
            if let Some(labels) = reply.get("errorLabels") {
                out.insert("errorLabels", labels.clone());
            }
            *reply = out;
            return;
        }
        // Done before the kill reached it: only the write-concern wait
        // sees it.
        if killed && wce && ok(reply) && !reply.contains_key("writeConcernError") {
            let ok_value = reply.remove("ok");
            reply.insert("writeConcernError", write_concern_error(true));
            if let Some(v) = ok_value {
                reply.insert("ok", v);
            }
        }
        return;
    }
    // A read. One whose interrupt was read as a value by the code that met
    // it (a filter that takes an error for a non-match, say) still ends
    // with the interrupt, as it does at mongod's interrupt point.
    if tripped
        && ok(reply)
        && matches!(
            name,
            "find" | "aggregate" | "count" | "distinct" | "getMore"
        )
    {
        let (code, code_name, _) = error_of(killed);
        let mut out = doc! {
            "ok": 0.0,
            "errmsg": secantus_core::deadline::MaxTimeMsExpired::MESSAGE,
            "code": code,
            "codeName": code_name,
        };
        wrap_read(name, doc, &mut out, ctx, killed);
        *reply = out;
        return;
    }
    if killed && code == Some(budget_code) {
        wrap_read(name, doc, reply, ctx, true);
        if wce {
            let mut out = doc! {"writeConcernError": write_concern_error(true)};
            for (k, v) in std::mem::take(reply) {
                out.insert(k, v);
            }
            *reply = out;
        }
    }
}

/// A read's interrupt under the executor wrapper mongod reports it in,
/// wherever it landed (measured 8.2.11: a killed find is "Executor error
/// during find command: <ns> :: caused by :: operation was interrupted").
fn wrap_read(name: &str, doc: &Document, reply: &mut Document, ctx: &CommandContext, killed: bool) {
    let (code, code_name, message) = error_of(killed);
    let errmsg = reply
        .get_str("errmsg")
        .unwrap_or(secantus_core::deadline::MaxTimeMsExpired::MESSAGE)
        .replace(secantus_core::deadline::MaxTimeMsExpired::MESSAGE, message);
    let errmsg = if errmsg.contains(" :: caused by :: ") {
        errmsg
    } else {
        let ns = |coll: &str| format!("{}.{coll}", ctx.db_name);
        match (name, doc.get_str(name)) {
            ("find", Ok(coll)) => {
                format!(
                    "Executor error during find command: {} :: caused by :: {errmsg}",
                    ns(coll)
                )
            }
            ("aggregate" | "count" | "distinct", Ok(coll)) => format!(
                "Executor error during {name} command on namespace: {} :: caused by :: {errmsg}",
                ns(coll)
            ),
            ("getMore", _) => format!("Executor error during getMore :: caused by :: {errmsg}"),
            _ => errmsg,
        }
    };
    reply.insert("errmsg", errmsg);
    reply.insert("code", code);
    reply.insert("codeName", code_name);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_write_concerns_wait() {
        let wc = |d: Document| Some(d);
        assert!(waits_for_write_concern(None, None), "the implicit majority");
        assert!(!waits_for_write_concern(wc(doc! {"w": 1}).as_ref(), None));
        assert!(!waits_for_write_concern(wc(doc! {"w": 1.0}).as_ref(), None));
        assert!(!waits_for_write_concern(
            wc(doc! {"w": 1, "wtimeout": 100}).as_ref(),
            None
        ));
        assert!(!waits_for_write_concern(
            wc(doc! {"w": 1, "j": false}).as_ref(),
            None
        ));
        assert!(waits_for_write_concern(
            wc(doc! {"w": 1, "j": true}).as_ref(),
            None
        ));
        assert!(waits_for_write_concern(
            wc(doc! {"w": 1, "fsync": true}).as_ref(),
            None
        ));
        assert!(waits_for_write_concern(
            wc(doc! {"w": "majority"}).as_ref(),
            None
        ));
        assert!(waits_for_write_concern(
            wc(doc! {"j": false}).as_ref(),
            None
        ));
        // A stored default of `w: 1` waits for nothing either.
        assert!(!waits_for_write_concern(None, Some(&doc! {"w": 1})));
        assert!(waits_for_write_concern(
            wc(doc! {"w": 2}).as_ref(),
            Some(&doc! {"w": 1})
        ));
    }

    #[test]
    fn the_error_names_the_interruption() {
        assert_eq!(
            error_of(true),
            (11601, "Interrupted", "operation was interrupted")
        );
        assert_eq!(
            error_of(false),
            (50, "MaxTimeMSExpired", "operation exceeded time limit")
        );
        let wce = write_concern_error(true);
        assert_eq!(wce.get_i32("code").unwrap(), 11601);
        assert_eq!(
            wce.get_document("errInfo").unwrap(),
            &doc! {"writeConcern": {"w": 1_i32, "wtimeout": 0_i32}}
        );
    }

    #[test]
    fn a_failed_statement_is_found_in_write_errors() {
        let reply =
            doc! {"n": 1, "writeErrors": [{"index": 1, "code": 50, "errmsg": "x"}], "ok": 1.0};
        assert_eq!(interrupted_statement(&reply), Some("x"));
        let dup =
            doc! {"n": 1, "writeErrors": [{"index": 1, "code": 11000, "errmsg": "x"}], "ok": 1.0};
        assert_eq!(interrupted_statement(&dup), None);
    }
}

//! MongoDB 8.0's client-level `bulkWrite`.
//!
//! Writes across several namespaces in one command. Runs ONLY against `admin`,
//! takes a flat `ops` list whose entries name a namespace by index into
//! `nsInfo`, and answers with a CURSOR of per-op results plus summary counters
//! -- not the `{n, writeErrors}` shape the single-collection write commands use.
//!
//! Every shape was probed against a live mongod 8.2.11 (2026-08-30). Mirrors
//! `_bulk_write` in `src/secantus/commands.py`, including the detail that cost
//! the Python side three spec failures: an op that FAILS must be reported
//! against that op, never as a command-level error, or a driver sees no partial
//! result at all.

use bson::{doc, Bson, Document};

use crate::{crud, CommandContext, CommandError, HandlerResult};

/// How many results fit the first batch: a COUNT limit and a SIZE limit.
///
/// `batchSize` is the obvious half. The other half is what the Go driver's
/// prose test 7 exercises: it sets NO batchSize and sends two upserts whose
/// `_id`s are each `maxBsonObjectSize / 2` bytes, so the two RESULT documents
/// cannot share one reply. mongod answers `firstBatch: 1` plus a cursor and the
/// driver asserts it made exactly one `getMore` (probed 8.2.11, 2026-09-18);
/// count-only batching returns both and the driver sees zero getMores.
///
/// An upserted `_id` is the only unbounded field a result carries, which is
/// what makes this reachable. At least one result is always taken -- a zero
/// would mean no progress is ever possible.
fn first_batch_len(results: &[Document], batch_size: i64) -> i64 {
    let budget = crate::MAX_BSON_OBJECT_SIZE as usize;
    let mut used: usize = 0;
    for (i, entry) in results.iter().enumerate() {
        if i as i64 >= batch_size {
            return i as i64;
        }
        used += bson::to_vec(entry).map(|v| v.len()).unwrap_or(0);
        if used > budget {
            return (i as i64).max(1);
        }
    }
    results.len() as i64
}

/// `bulkWrite` results live under the admin command namespace, which is also
/// what `getMore`'s `collection: "$cmd.bulkWrite"` resolves to.
const BULK_WRITE_NS: &str = "admin.$cmd.bulkWrite";

const KNOWN_FIELDS: &[&str] = &[
    "bulkWrite",
    "ops",
    "nsInfo",
    "ordered",
    "bypassDocumentValidation",
    // Accepted by mongod 8.2.11 on `bulkWrite`, `insert` AND `update` (probed
    // 2026-09-18). Both servers already took it on `insert` / `update` and only
    // `bulkWrite` refused it, which failed all 17 of the Go driver's
    // `TestClient_BulkWrite_AddCommandFields` cases. Accepted and IGNORED: the
    // flag governs whether an empty `Timestamp()` is replaced with the current
    // cluster time, and neither server implements that substitution -- the go
    // gauge deselects `TestBypassEmptyTsReplacement` for exactly that reason.
    "bypassEmptyTsReplacement",
    "let",
    "errorsOnly",
    "comment",
    "cursor",
    "maxTimeMS",
    "writeConcern",
    "lsid",
    "txnNumber",
    "autocommit",
    "startTransaction",
    // Stable API envelope. Omitting these rejected the fields pymongo appends
    // when a client declares an API version, so `client.bulk_write()` failed
    // outright under Stable API -- caught by
    // `test_client_bulkWrite_appends_declared_API_version`. `$`-prefixed
    // envelope keys are allowed unconditionally below.
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
];

/// mongod's batch bounds, quoted in its own InvalidLength message.
const MAX_OPS: usize = 100_000;

fn bad_value(msg: String) -> Document {
    CommandError::new(2, "BadValue", msg).into_reply()
}

/// A failed op's cursor entry. mongod leads with `ok`/`idx`/`code`.
fn op_error(idx: usize, code: i32, errmsg: &str, extra: Option<&Document>) -> Document {
    let mut out = doc! { "ok": 0.0, "idx": idx as i32, "code": code, "errmsg": errmsg };
    if let Some(src) = extra {
        for key in ["keyPattern", "keyValue"] {
            if let Some(v) = src.get(key) {
                out.insert(key, v.clone());
            }
        }
    }
    out.insert("n", 0i32);
    out
}

/// Render an op for an error message, mongod-style.
fn render_op(op: &Document) -> String {
    crate::argtypes::render_stage_value(&Bson::Document(op.clone()))
}

pub fn bulk_write(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if ctx.db_name != "admin" {
        return Ok(CommandError::new(
            13,
            "Unauthorized",
            "bulkWrite may only be run against the admin database.",
        )
        .into_reply());
    }
    if let Some(unknown) = doc
        .keys()
        .find(|k| !k.starts_with('$') && !KNOWN_FIELDS.contains(&k.as_str()))
    {
        return Ok(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'bulkWrite.{unknown}' is an unknown field."),
        )
        .into_reply());
    }

    let ops = match doc.get_array("ops") {
        Ok(a) if !a.is_empty() && a.len() <= MAX_OPS => a.clone(),
        other => {
            let n = other.map(|a| a.len()).unwrap_or(0);
            return Ok(CommandError::new(
                16,
                "InvalidLength",
                format!("Write batch sizes must be between 1 and {MAX_OPS}. Got {n} operations."),
            )
            .into_reply());
        }
    };
    let Ok(ns_info) = doc.get_array("nsInfo") else {
        return Ok(bad_value(
            "BSON field 'bulkWrite.nsInfo' is the wrong type".to_string(),
        ));
    };

    let ordered = doc.get_bool("ordered").unwrap_or(true);
    let errors_only = doc.get_bool("errorsOnly").unwrap_or(false);
    let mut results: Vec<Document> = Vec::new();
    let (mut n_inserted, mut n_matched, mut n_modified, mut n_upserted, mut n_deleted) =
        (0i32, 0i32, 0i32, 0i32, 0i32);
    let mut n_errors = 0i32;

    let outer_db = ctx.db_name.clone();
    for (idx, raw) in ops.iter().enumerate() {
        let Some(op) = raw.as_document() else {
            return Ok(bad_value(format!(
                "BulkWrite ops entry {} is not an object",
                crate::argtypes::render_stage_value(raw)
            )));
        };
        let kind = ["insert", "update", "delete"]
            .into_iter()
            .find(|k| op.contains_key(*k));
        let Some(kind) = kind else {
            return Ok(bad_value(format!(
                "BulkWrite ops entry {} does not contain a supported operation type",
                render_op(op)
            )));
        };
        let ns_index = match op.get(kind) {
            Some(Bson::Int32(i)) => *i as i64,
            Some(Bson::Int64(i)) => *i,
            _ => -1,
        };
        let entry = if ns_index >= 0 {
            ns_info.get(ns_index as usize).and_then(|b| b.as_document())
        } else {
            None
        };
        let ns = entry.and_then(|d| d.get_str("ns").ok()).unwrap_or("");
        if ns.is_empty() || !ns.contains('.') {
            return Ok(bad_value(format!(
                "BulkWrite ops entry {} has an invalid nsInfo index.",
                render_op(op)
            )));
        }
        let (db_name, coll) = match ns.split_once('.') {
            Some((d, c)) => (d.to_string(), c.to_string()),
            None => unreachable!("checked above"),
        };

        let cmd = match kind {
            "insert" => {
                let Some(document) = op.get("document") else {
                    return Ok(missing("bulkWrite.ops.document"));
                };
                doc! { "insert": &coll, "documents": [document.clone()] }
            }
            "update" => {
                let Some(mods) = op.get("updateMods") else {
                    return Ok(missing("bulkWrite.ops.updateMods"));
                };
                let mut stmt = doc! {
                    "q": op.get("filter").cloned().unwrap_or(Bson::Document(Document::new())),
                    "u": mods.clone(),
                    "multi": op.get_bool("multi").unwrap_or(false),
                };
                for key in ["upsert", "arrayFilters", "hint", "collation", "sort"] {
                    if let Some(v) = op.get(key) {
                        stmt.insert(key, v.clone());
                    }
                }
                doc! { "update": &coll, "updates": [Bson::Document(stmt)] }
            }
            _ => {
                let Some(filter) = op.get("filter") else {
                    return Ok(missing("bulkWrite.ops.filter"));
                };
                let mut stmt = doc! {
                    "q": filter.clone(),
                    "limit": if op.get_bool("multi").unwrap_or(false) { 0i32 } else { 1i32 },
                };
                for key in ["hint", "collation"] {
                    if let Some(v) = op.get(key) {
                        stmt.insert(key, v.clone());
                    }
                }
                doc! { "delete": &coll, "deletes": [Bson::Document(stmt)] }
            }
        };
        let mut cmd = cmd;
        if let Some(v) = doc.get("let") {
            cmd.insert("let", v.clone());
        }
        if let Some(v) = doc.get("bypassDocumentValidation") {
            cmd.insert("bypassDocumentValidation", v.clone());
        }

        // Run the op through the ordinary single-write handler, with the
        // context rebound to that op's database, so bulk semantics cannot drift
        // from single-write semantics.
        ctx.db_name = db_name;
        let reply = match kind {
            "insert" => crud::insert(&cmd, ctx),
            "update" => crud::update(&cmd, ctx),
            _ => crud::delete(&cmd, ctx),
        };
        ctx.db_name = outer_db.clone();

        let reply = match reply {
            Ok(r) => r,
            Err(err) => {
                // A per-op failure, NOT a command failure: letting it escape
                // would fail the whole batch and leave the driver with no
                // partial result.
                results.push(op_error(idx, err.code, &err.errmsg, None));
                n_errors += 1;
                if ordered {
                    break;
                }
                continue;
            }
        };
        if reply.get_f64("ok").unwrap_or(1.0) == 0.0 {
            let code = reply.get_i32("code").unwrap_or(8);
            let msg = reply.get_str("errmsg").unwrap_or("").to_string();
            results.push(op_error(idx, code, &msg, Some(&reply)));
            n_errors += 1;
            if ordered {
                break;
            }
            continue;
        }
        if let Ok(write_errors) = reply.get_array("writeErrors") {
            if let Some(werr) = write_errors.first().and_then(|b| b.as_document()) {
                let code = werr.get_i32("code").unwrap_or(8);
                let msg = werr.get_str("errmsg").unwrap_or("").to_string();
                results.push(op_error(idx, code, &msg, Some(werr)));
                n_errors += 1;
                if ordered {
                    break;
                }
                continue;
            }
        }

        let n = reply.get_i32("n").unwrap_or(0);
        let mut entry_out = doc! { "ok": 1.0, "idx": idx as i32, "n": n };
        match kind {
            "insert" => n_inserted += n,
            "update" => {
                let modified = reply.get_i32("nModified").unwrap_or(0);
                entry_out.insert("nModified", modified);
                let upserted = reply
                    .get_array("upserted")
                    .ok()
                    .cloned()
                    .unwrap_or_default();
                if let Some(first) = upserted.first().and_then(|b| b.as_document()) {
                    n_upserted += upserted.len() as i32;
                    if let Some(id) = first.get("_id") {
                        entry_out.insert("upserted", doc! { "_id": id.clone() });
                    }
                } else {
                    n_matched += n;
                }
                n_modified += modified;
            }
            _ => n_deleted += n,
        }
        if !errors_only {
            results.push(entry_out);
        }
    }

    // The results are a real CURSOR when they do not fit the requested batch,
    // exactly like `find` / `aggregate`. Measured against mongod 8.2.11
    // (2026-09-18); the boundary is strictly "more remain", not ">=":
    //
    //     no `cursor` option        id 0, every result in firstBatch
    //     batchSize 2, 5 results    id SET, firstBatch 2, getMore -> 3
    //     batchSize 2, 2 results    id 0   (an exact fit keeps no cursor)
    //     batchSize 0, 5 results    id SET, firstBatch EMPTY, getMore -> 5
    //     errorsOnly, no errors     id 0   (nothing to return, nothing to page)
    //
    // `bounded: true` is what encodes that exact-fit rule -- an unbounded
    // cursor deliberately stays open after filling a batch exactly, which is
    // `find`'s behaviour and NOT this one. An absent `cursor.batchSize` means
    // unbounded here, so it is passed as the full result length rather than
    // `find`'s default batch.
    //
    // `getMore` addresses it as `{getMore: <id>, collection: "$cmd.bulkWrite"}`
    // against ADMIN, which is the namespace registered below.
    let batch_size = doc
        .get_document("cursor")
        .ok()
        .and_then(|c| match c.get("batchSize") {
            Some(Bson::Int32(n)) => Some(i64::from(*n)),
            Some(Bson::Int64(n)) => Some(*n),
            _ => None,
        })
        .map(|n| n.max(0))
        .unwrap_or(results.len() as i64);
    let batch_size = first_batch_len(&results, batch_size);
    let (first_batch, cursor_id) = crate::find::split_docs_into_cursor(
        results,
        batch_size,
        BULK_WRITE_NS,
        ctx.cursors()?,
        true,
    )?;

    // Field order is mongod's: the cursor first, then the counters, then `ok`.
    Ok(doc! {
        "cursor": doc! {
            "id": cursor_id,
            "firstBatch": first_batch,
            "ns": BULK_WRITE_NS,
        },
        "nErrors": n_errors,
        "nInserted": n_inserted,
        "nMatched": n_matched,
        "nModified": n_modified,
        "nUpserted": n_upserted,
        "nDeleted": n_deleted,
        "ok": 1.0,
    })
}

/// mongod's `IDLFailedToParse` for a required op field that is absent.
fn missing(field: &str) -> Document {
    CommandError::new(
        40414,
        "IDLFailedToParse",
        format!("BSON field '{field}' is missing but a required field"),
    )
    .into_reply()
}

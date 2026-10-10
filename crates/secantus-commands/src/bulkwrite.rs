//! MongoDB 8.0's client-level `bulkWrite`.
//!
//! Writes across several namespaces in one command. Runs ONLY against `admin`,
//! takes a flat `ops` list whose entries name a namespace by index into
//! `nsInfo`, and answers with a CURSOR of per-op results plus summary counters
//! -- not the `{n, writeErrors}` shape the single-collection write commands use.
//!
//! The whole command is checked before any op runs, in mongod's order; the
//! checks and their messages are `tools/probes/bulk_write_command.py`'s 141
//! scenarios against mongod 8.2.11 (2026-10-10).
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

/// A failed op's cursor entry. mongod leads with `ok`/`idx`/`code`, carries a
/// duplicate key's `keyPattern` / `keyValue` and a failed validation's
/// `errInfo`, and ends with the counters an op of that kind reports.
fn op_error(
    idx: usize,
    code: i32,
    errmsg: &str,
    extra: Option<&Document>,
    is_update: bool,
) -> Document {
    let mut out = doc! { "ok": 0.0, "idx": idx as i32, "code": code, "errmsg": errmsg };
    if let Some(src) = extra {
        for key in ["keyPattern", "keyValue", "errInfo"] {
            if let Some(v) = src.get(key) {
                out.insert(key, v.clone());
            }
        }
    }
    out.insert("n", 0i32);
    if is_update {
        out.insert("nModified", 0i32);
    }
    out
}

/// Render an op for an error message, mongod-style.
fn render_op(op: &Document) -> String {
    crate::argtypes::render_stage_value(&Bson::Document(op.clone()))
}

/// What an op field must be, for [`op_field_problem`].
#[derive(Clone, Copy)]
enum Want {
    Index,
    Object,
    Bool,
    Array,
    UpdateMods,
    Hint,
}

/// The fields each op kind takes, in mongod's declaration order (the order a
/// missing required field is reported in). Measured 8.2.11, 2026-10-10.
fn op_fields(kind: &str) -> &'static [(&'static str, Want, bool)] {
    match kind {
        "insert" => &[
            ("insert", Want::Index, true),
            ("document", Want::Object, true),
        ],
        "update" => &[
            ("update", Want::Index, true),
            ("filter", Want::Object, true),
            ("sort", Want::Object, false),
            ("multi", Want::Bool, false),
            ("updateMods", Want::UpdateMods, true),
            ("upsert", Want::Bool, false),
            ("upsertSupplied", Want::Bool, false),
            ("arrayFilters", Want::Array, false),
            ("hint", Want::Hint, false),
            ("constants", Want::Object, false),
            ("collation", Want::Object, false),
        ],
        _ => &[
            ("delete", Want::Index, true),
            ("filter", Want::Object, true),
            ("multi", Want::Bool, false),
            ("hint", Want::Hint, false),
            ("collation", Want::Object, false),
        ],
    }
}

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected {expected}",
            secantus_core::query::bson_type_name(value)
        ),
    )
}

fn unknown_field(path: &str) -> CommandError {
    CommandError::new(
        40415,
        "IDLUnknownField",
        format!("BSON field '{path}' is an unknown field."),
    )
}

fn missing_field(path: &str) -> CommandError {
    CommandError::new(
        40414,
        "IDLFailedToParse",
        format!("BSON field '{path}' is missing but a required field"),
    )
}

/// A whole number from any of the four numeric types mongod takes for a count
/// or an index.
fn whole_number(value: &Bson) -> Option<i64> {
    match value {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        Bson::Decimal128(d) => d.to_string().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

/// The first thing wrong with one op, read the way mongod's parser reads it:
/// the first field names the kind, every field is checked where it stands,
/// and a required field that never appeared is reported last.
fn op_problem(op: &Document) -> Result<&'static str, CommandError> {
    let first = op.keys().next().map(String::as_str).unwrap_or("");
    let kind = match first {
        "insert" => "insert",
        "update" => "update",
        "delete" => "delete",
        other => return Err(unknown_field(&format!("bulkWrite.{other}"))),
    };
    let fields = op_fields(kind);
    for (name, value) in op {
        let Some((_, want, _)) = fields.iter().find(|(f, _, _)| f == name) else {
            return Err(unknown_field(&format!("bulkWrite.ops.{name}")));
        };
        let path = format!("bulkWrite.ops.{name}");
        match want {
            Want::Index => match whole_number(value) {
                Some(n) if n < 0 => {
                    return Err(CommandError::new(
                        2,
                        "BadValue",
                        format!("BSON field '{name}' value must be >= 0, actual value '{n}'"),
                    ))
                }
                Some(_) => {}
                None => {
                    return Err(wrong_type(
                        &path,
                        value,
                        "types '[long, int, decimal, double]'",
                    ))
                }
            },
            Want::Object if !matches!(value, Bson::Document(_)) => {
                return Err(wrong_type(&path, value, "type 'object'"))
            }
            Want::Bool if !matches!(value, Bson::Boolean(_)) => {
                return Err(wrong_type(&path, value, "type 'bool'"))
            }
            Want::Array if !matches!(value, Bson::Array(_)) => {
                return Err(wrong_type(&path, value, "type 'array'"))
            }
            Want::UpdateMods if !matches!(value, Bson::Document(_) | Bson::Array(_)) => {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    "Update argument must be either an object or an array",
                ))
            }
            Want::Hint if !matches!(value, Bson::Document(_) | Bson::String(_)) => {
                return Err(CommandError::new(
                    9,
                    "FailedToParse",
                    "Hint must be a string or an object",
                ))
            }
            _ => {}
        }
    }
    for (name, _, required) in fields {
        if *required && !op.contains_key(*name) {
            return Err(missing_field(&format!("bulkWrite.ops.{name}")));
        }
    }
    Ok(kind)
}

/// The first thing wrong with the command's own fields, in the order sent.
fn body_problem(doc: &Document) -> Result<(), CommandError> {
    for (name, value) in doc {
        if name.starts_with('$') {
            continue;
        }
        if !KNOWN_FIELDS.contains(&name.as_str()) {
            return Err(unknown_field(&format!("bulkWrite.{name}")));
        }
        let path = format!("bulkWrite.{name}");
        match name.as_str() {
            "ordered" | "errorsOnly" if !matches!(value, Bson::Boolean(_)) => {
                return Err(wrong_type(&path, value, "type 'bool'"))
            }
            "bypassDocumentValidation"
                if !matches!(value, Bson::Boolean(_)) && whole_number(value).is_none() =>
            {
                return Err(wrong_type(
                    &path,
                    value,
                    "types '[bool, long, int, decimal, double]'",
                ))
            }
            "let" if !matches!(value, Bson::Document(_)) => {
                return Err(wrong_type(&path, value, "type 'object'"))
            }
            "cursor" => {
                let Bson::Document(cursor) = value else {
                    return Err(wrong_type(&path, value, "type 'object'"));
                };
                for (key, v) in cursor {
                    if key != "batchSize" {
                        return Err(unknown_field(&format!("bulkWrite.cursor.{key}")));
                    }
                    match whole_number(v) {
                        Some(n) if n < 0 => {
                            return Err(CommandError::new(
                                2,
                                "BadValue",
                                format!(
                                    "BSON field 'batchSize' value must be >= 0, actual value \
                                     '{n}'"
                                ),
                            ))
                        }
                        Some(_) => {}
                        None => {
                            return Err(wrong_type(
                                "bulkWrite.cursor.batchSize",
                                v,
                                "types '[long, int, decimal, double]'",
                            ))
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The first thing wrong with one `nsInfo` entry's fields.
fn ns_info_problem(entry: &Document) -> Result<(), CommandError> {
    for (name, value) in entry {
        let path = format!("bulkWrite.nsInfo.{name}");
        match name.as_str() {
            "ns" if !matches!(value, Bson::String(_)) => {
                return Err(wrong_type(&path, value, "type 'string'"))
            }
            "collectionUUID" if !matches!(value, Bson::Binary(_)) => {
                return Err(wrong_type(&path, value, "type 'binData'"))
            }
            "encryptionInformation" if !matches!(value, Bson::Document(_)) => {
                return Err(wrong_type(&path, value, "type 'object'"))
            }
            "isTimeseriesNamespace" if !matches!(value, Bson::Boolean(_)) => {
                return Err(wrong_type(&path, value, "type 'bool'"))
            }
            "ns"
            | "collectionUUID"
            | "encryptionInformation"
            | "isTimeseriesNamespace"
            | "shardVersion"
            | "databaseVersion" => {}
            _ => return Err(unknown_field(&path)),
        }
    }
    if !entry.contains_key("ns") {
        return Err(missing_field("bulkWrite.nsInfo.ns"));
    }
    Ok(())
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
    // Everything below up to the first write is CHECKED BEFORE ANYTHING IS
    // WRITTEN, in mongod's order (measured 8.2.11, 2026-10-10): the command's
    // own fields, each `nsInfo` entry, each op, the namespaces, each op's
    // namespace index, and last the namespaces the ops actually write. A
    // malformed third op used to leave the first two applied.
    if let Err(e) = body_problem(doc) {
        return Ok(e.into_reply());
    }
    let Some(ns_value) = doc.get("nsInfo") else {
        return Ok(missing_field("bulkWrite.nsInfo").into_reply());
    };
    let Some(ops_value) = doc.get("ops") else {
        return Ok(missing_field("bulkWrite.ops").into_reply());
    };
    let Bson::Array(ns_info) = ns_value else {
        return Ok(wrong_type("bulkWrite.nsInfo", ns_value, "type 'array'").into_reply());
    };
    let Bson::Array(ops) = ops_value else {
        return Ok(wrong_type("bulkWrite.ops", ops_value, "type 'array'").into_reply());
    };
    for entry in ns_info {
        let Bson::Document(entry) = entry else {
            return Ok(wrong_type("bulkWrite.nsInfo", entry, "type 'object'").into_reply());
        };
        if let Err(e) = ns_info_problem(entry) {
            return Ok(e.into_reply());
        }
    }
    let mut kinds: Vec<&'static str> = Vec::with_capacity(ops.len());
    for raw in ops {
        let Bson::Document(op) = raw else {
            return Ok(wrong_type("bulkWrite.ops", raw, "type 'object'").into_reply());
        };
        match op_problem(op) {
            Ok(kind) => kinds.push(kind),
            Err(e) => return Ok(e.into_reply()),
        }
    }
    if ops.is_empty() || ops.len() > MAX_OPS {
        return Ok(CommandError::new(
            16,
            "InvalidLength",
            format!(
                "Write batch sizes must be between 1 and {MAX_OPS}. Got {} operations.",
                ops.len()
            ),
        )
        .into_reply());
    }
    // Every `nsInfo` entry must name a database and a collection, whether or
    // not an op uses it.
    let mut namespaces: Vec<(String, String)> = Vec::with_capacity(ns_info.len());
    for entry in ns_info {
        let ns = entry
            .as_document()
            .and_then(|d| d.get_str("ns").ok())
            .unwrap_or("");
        match ns.split_once('.') {
            Some((db, coll)) if !db.is_empty() && !coll.is_empty() => {
                namespaces.push((db.to_string(), coll.to_string()));
            }
            split => {
                // mongod prints a namespace with no collection as its database.
                let shown = match split {
                    Some((db, "")) => db,
                    _ => ns,
                };
                return Ok(CommandError::new(
                    73,
                    "InvalidNamespace",
                    format!("Invalid namespace specified for bulkWrite: '{shown}'"),
                )
                .into_reply());
            }
        }
    }
    let mut targets: Vec<usize> = Vec::with_capacity(ops.len());
    for (raw, kind) in ops.iter().zip(&kinds) {
        let op = raw.as_document().expect("checked above");
        let index = op.get(*kind).and_then(whole_number).unwrap_or(-1);
        if index < 0 || index as usize >= namespaces.len() {
            return Ok(bad_value(format!(
                "BulkWrite ops entry {} has an invalid nsInfo index.",
                render_op(op)
            )));
        }
        targets.push(index as usize);
    }
    // A namespace no write command may touch fails the COMMAND, but only when
    // an op writes to it.
    for target in &targets {
        let (db, coll) = &namespaces[*target];
        if let Some(e) = crate::admin::invalid_write_namespace(db, coll) {
            return Ok(e.into_reply());
        }
    }

    let ordered = doc.get_bool("ordered").unwrap_or(true);
    let errors_only = doc.get_bool("errorsOnly").unwrap_or(false);
    // An unacknowledged write reports its counters and no per-op results,
    // errors included.
    let unacknowledged = matches!(
        doc.get_document("writeConcern").ok().and_then(|w| w.get("w")),
        Some(w) if whole_number(w) == Some(0)
    );
    let mut results: Vec<Document> = Vec::new();
    let (mut n_inserted, mut n_matched, mut n_modified, mut n_upserted, mut n_deleted) =
        (0i32, 0i32, 0i32, 0i32, 0i32);
    let mut n_errors = 0i32;

    let outer_db = ctx.db_name.clone();
    for (idx, raw) in ops.iter().enumerate() {
        let op = raw.as_document().expect("checked above");
        let kind = kinds[idx];
        let (db_name, coll) = namespaces[targets[idx]].clone();
        let is_update = kind == "update";

        // A view takes no writes: this op fails, and the batch goes on or
        // stops by `ordered` like any other failed op. The single-write
        // commands are refused in dispatch, which this command's ops bypass.
        if ctx
            .storage()
            .map(|s| crate::views::is_view(s, &db_name, &coll))
            .unwrap_or(false)
        {
            results.push(op_error(
                idx,
                166,
                &format!("Namespace {db_name}.{coll} is a view, not a collection"),
                None,
                is_update,
            ));
            n_errors += 1;
            if ordered {
                break;
            }
            continue;
        }

        let cmd = match kind {
            "insert" => {
                let document = op.get("document").cloned().unwrap_or(Bson::Null);
                doc! { "insert": &coll, "documents": [document] }
            }
            "update" => {
                let mut stmt = doc! {
                    "q": op.get("filter").cloned().unwrap_or(Bson::Document(Document::new())),
                    "u": op.get("updateMods").cloned().unwrap_or(Bson::Null),
                    "multi": op.get_bool("multi").unwrap_or(false),
                };
                for key in [
                    "upsert",
                    "upsertSupplied",
                    "arrayFilters",
                    "hint",
                    "collation",
                    "sort",
                ] {
                    if let Some(v) = op.get(key) {
                        stmt.insert(key, v.clone());
                    }
                }
                if let Some(v) = op.get("constants") {
                    stmt.insert("c", v.clone());
                }
                doc! { "update": &coll, "updates": [Bson::Document(stmt)] }
            }
            _ => {
                let mut stmt = doc! {
                    "q": op.get("filter").cloned().unwrap_or(Bson::Document(Document::new())),
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
                results.push(op_error(idx, err.code, &err.errmsg, None, is_update));
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
            results.push(op_error(idx, code, &msg, Some(&reply), is_update));
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
                results.push(op_error(idx, code, &msg, Some(werr), is_update));
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
    if unacknowledged {
        results.clear();
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
        .and_then(|c| c.get("batchSize").and_then(whole_number))
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

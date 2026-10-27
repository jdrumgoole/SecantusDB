//! `applyOps` (RM-219): apply oplog entries as mongod 8.2.11's `applyOps`
//! applies them, which is what `mongorestore --oplogReplay` sends.
//!
//! Each entry is written through the ordinary write paths, so it is logged as
//! a write of its own, and with mongod's oplog-application rules: an insert
//! of an `_id` that exists replaces the document, an update of a document
//! that does not exist fails unless the entry says `b: true`, a delete of one
//! that does not exist succeeds, and a command that is already true (create
//! a collection that exists, drop one that does not) succeeds.
//!
//! Three kinds of failure, measured (2026-10-20, one-member replica set):
//! - a malformed entry list (an entry not an object, `op` not a string, `ns`
//!   missing or not a namespace, a command `applyOps` does not know) refuses
//!   the command before anything is applied;
//! - an entry that raises an error stops there, reporting that error with
//!   `applied` (entries tried) and `results` (one flag per entry tried);
//! - an entry that simply does not apply (an update of a missing document, a
//!   document the validator refuses, a `ui` naming another collection) is
//!   `false` in `results` and the rest go on; the command then fails with 8
//!   "applyOps had one or more errors applying ops".
//!
//! An update's `o` is mongod's own `$v: 2` diff (`u` / `i` / `d` sections,
//! `s<field>` sub-diffs, array diffs with `a` / `l`), applied here by
//! [`apply_diff`].

use bson::{doc, Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

/// The fields an oplog entry may carry (mongod's `OplogEntryBase` and the
/// durable replicated-operation fields).
const ENTRY_FIELDS: &[&str] = &[
    "op",
    "ns",
    "ui",
    "o",
    "o2",
    "b",
    "ts",
    "t",
    "v",
    "wall",
    "h",
    "lsid",
    "txnNumber",
    "stmtId",
    "prevOpTime",
    "preImageOpTime",
    "postImageOpTime",
    "needsRetryImage",
    "fromMigrate",
    "_id",
    "tid",
    "recordId",
    "versionContext",
    "multiOpType",
    "checkExistenceForDiffInsert",
    "destinedRecipient",
    "fromTenantMigration",
];

/// The operation types an entry's `op` may name.
const OP_TYPES: &[&str] = &["i", "u", "d", "c", "n", "km", "xi", "xd"];

/// The commands a `c` entry may carry (mongod's `commandString` enum).
const ENTRY_COMMANDS: &[&str] = &[
    "create",
    "createIndexes",
    "startIndexBuild",
    "commitIndexBuild",
    "abortIndexBuild",
    "drop",
    "collMod",
    "dropDatabase",
    "dropIndexes",
    "deleteIndex",
    "deleteIndexes",
    "renameCollection",
    "applyOps",
    "commitTransaction",
    "abortTransaction",
    "importCollection",
    "createDatabaseMetadata",
    "modifyCollectionShardingIndexCatalog",
];

/// The commands this server applies; the others in `ENTRY_COMMANDS` are
/// refused by name.
const APPLIED_COMMANDS: &[&str] = &[
    "create",
    "drop",
    "collMod",
    "dropDatabase",
    "dropIndexes",
    "deleteIndex",
    "deleteIndexes",
    "renameCollection",
    "applyOps",
];

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

fn error(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

fn render(v: &Bson) -> String {
    crate::argtypes::render_stage_value(v)
}

/// What one entry came to.
enum Outcome {
    Applied,
    /// Did not apply; the rest go on.
    NotApplied,
}

/// `applyOps`.
pub fn apply_ops(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let entries = match doc.get("applyOps") {
        Some(Bson::Array(a)) => a,
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "Wrong type for 'applyOps'. Expected a array, got a {}.",
                    type_name(other)
                ),
            ))
        }
        None => {
            return Err(error(
                14,
                "Wrong type for 'applyOps'. Expected a array, got a missing.",
            ))
        }
    };
    check_command_options(doc)?;
    // The whole list is read before anything is applied.
    let mut parsed: Vec<&Document> = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let Bson::Document(entry) = e else {
            return Err(error(
                14,
                format!(
                    "Wrong type for '{i}'. Expected a object, got a {}.",
                    type_name(e)
                ),
            ));
        };
        check_entry_shape(entry, i)?;
        parsed.push(entry);
    }
    let bypass_validation = doc
        .get("bypassDocumentValidation")
        .is_some_and(crate::util::truthy);
    let mut results: Vec<Bson> = Vec::new();
    let mut all_applied = true;
    for entry in parsed {
        if entry.get_str("op") == Ok("n") {
            continue;
        }
        match apply_entry(entry, bypass_validation, ctx) {
            Ok(Outcome::Applied) => results.push(Bson::Boolean(true)),
            Ok(Outcome::NotApplied) => {
                all_applied = false;
                results.push(Bson::Boolean(false));
            }
            Err(e) => {
                results.push(Bson::Boolean(false));
                let mut reply = doc! {
                    "applied": results.len() as i32,
                    "code": e.code,
                    "codeName": e.code_name.clone(),
                    "errmsg": e.errmsg.clone(),
                    "results": results,
                    "ok": 0.0,
                };
                if let Some(extra) = e.extra.clone() {
                    reply.extend(*extra);
                }
                return Ok(reply);
            }
        }
    }
    let applied = results.len() as i32;
    if all_applied {
        Ok(doc! {"applied": applied, "results": results, "ok": 1.0})
    } else {
        Ok(doc! {
            "applied": applied,
            "results": results,
            "ok": 0.0,
            "errmsg": "applyOps had one or more errors applying ops",
            "code": 8,
            "codeName": "UnknownError",
        })
    }
}

/// The command's own options: two that mongod no longer takes, and the
/// application mode.
fn check_command_options(doc: &Document) -> Result<(), CommandError> {
    for (option, code) in [("alwaysUpsert", 6711601), ("preCondition", 6711600)] {
        if doc.contains_key(option) {
            return Err(error(
                code,
                format!(
                    "Failed to parse applyOps command: {} :: caused by :: applyOps command no \
                     longer supports the '{option}' option",
                    render(&Bson::Document(doc.clone()))
                ),
            ));
        }
    }
    if let Some(mode) = doc.get("oplogApplicationMode") {
        let known = matches!(
            mode.as_str(),
            Some("InitialSync" | "Recovering" | "Secondary" | "ApplyOps")
        );
        if !known {
            let shown = match mode {
                Bson::String(s) => s.clone(),
                other => render(other),
            };
            return Err(error(
                9,
                format!(
                    "Could not parse oplogApplicationMode :: caused by :: Invalid oplog \
                     application mode provided: {shown}"
                ),
            ));
        }
    }
    Ok(())
}

/// What mongod reads of each entry before it applies any.
fn check_entry_shape(entry: &Document, i: usize) -> Result<(), CommandError> {
    let op = match entry.get("op") {
        Some(Bson::String(s)) => s.as_str(),
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "Wrong type for 'op'. Expected a string, got a {}.",
                    type_name(other)
                ),
            ))
        }
        // mongod's own text, the empty name included.
        None => {
            return Err(error(
                4,
                "Wrong type for ''. Expected a string, got a missing.",
            ))
        }
    };
    let ns = match entry.get("ns") {
        Some(Bson::String(s)) => s.as_str(),
        Some(_) => return Err(error(20, format!("\"ns\" field is not a string: {i}"))),
        None => {
            return Err(error(
                20,
                format!("op does not contain required \"ns\" field: {i}"),
            ))
        }
    };
    if op != "n" && !ns.contains('.') {
        return Err(error(73, format!("invalid ns: {ns}")));
    }
    if op == "c" {
        if let Some(name) = entry.get_document("o").ok().and_then(|o| o.keys().next()) {
            if !ENTRY_COMMANDS.contains(&name.as_str()) {
                return Err(error(
                    2,
                    format!(
                        "Enumeration value '{name}' for field 'commandString' is not a valid \
                         value."
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// The entry as mongod quotes it in a missing-`_id` error: with the `ts` and
/// `wall` it is given when it has none.
fn quoted_entry(entry: &Document) -> String {
    let mut shown = entry.clone();
    if !shown.contains_key("ts") {
        shown.insert(
            "ts",
            bson::Timestamp {
                time: 0,
                increment: 0,
            },
        );
    }
    if !shown.contains_key("wall") {
        shown.insert("wall", bson::DateTime::from_millis(0));
    }
    format!("{{ oplogEntry: {} }}", render(&Bson::Document(shown)))
}

fn split_ns(ns: &str) -> (&str, &str) {
    ns.split_once('.').unwrap_or((ns, ""))
}

/// Apply one entry.
fn apply_entry(
    entry: &Document,
    bypass_validation: bool,
    ctx: &mut CommandContext,
) -> Result<Outcome, CommandError> {
    for k in entry.keys() {
        if !ENTRY_FIELDS.contains(&k.as_str()) {
            return Err(error(
                40415,
                format!("BSON field 'OplogEntryBase.{k}' is an unknown field."),
            ));
        }
    }
    let op = entry.get_str("op").unwrap_or("");
    if !OP_TYPES.contains(&op) {
        return Err(error(
            2,
            format!("Enumeration value '{op}' for field 'OplogEntryBase.op' is not a valid value."),
        ));
    }
    let o = match entry.get("o") {
        Some(Bson::Document(o)) => o,
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'OplogEntryBase.o' is the wrong type '{}', expected type 'object'",
                    type_name(other)
                ),
            ))
        }
        None => {
            return Err(error(
                40414,
                "BSON field 'OplogEntryBase.o' is missing but a required field",
            ))
        }
    };
    let ns = entry.get_str("ns").unwrap_or("");
    let (db, coll) = split_ns(ns);
    if op == "c" {
        return apply_command(db, o, ctx);
    }
    if !matches!(op, "i" | "u" | "d") {
        return Err(error(
            115,
            format!("applyOps of an '{op}' entry is not supported"),
        ));
    }
    let storage = ctx
        .storage
        .clone()
        .ok_or_else(|| error(1, "storage backend not configured"))?;
    if crate::views::is_view(storage.as_ref(), db, coll)
        || is_timeseries(storage.as_ref(), db, coll)
    {
        return Err(error(
            166,
            format!("Namespace {ns} is a view, not a collection"),
        ));
    }
    let exists = storage
        .collection_exists(db, coll)
        .map_err(crate::util::command_error)?;
    if let Some(Bson::Binary(ui)) = entry.get("ui") {
        let matches = exists
            && storage
                .collection_uuid(db, coll)
                .is_ok_and(|uuid| uuid == ui.bytes);
        if !matches {
            return Ok(Outcome::NotApplied);
        }
    }
    let missing_ns = || {
        error(
            26,
            format!(
                "cannot apply insert or update operation on a non-existent namespace {ns}: {}",
                render(&Bson::Document(entry.clone()))
            ),
        )
    };
    let validator = if bypass_validation {
        None
    } else {
        collection_validator(storage.as_ref(), db, coll)
    };
    match op {
        "i" => {
            if !exists {
                return Err(missing_ns());
            }
            let Some(id) = o.get("_id").cloned() else {
                return Err(error(
                    4,
                    format!(
                        "Failed to apply insert due to missing _id: {}",
                        quoted_entry(entry)
                    ),
                ));
            };
            write_document(
                storage.as_ref(),
                db,
                coll,
                &id,
                o.clone(),
                validator.as_ref(),
            )
        }
        "u" => {
            let Some(id) = entry
                .get_document("o2")
                .ok()
                .and_then(|o2| o2.get("_id"))
                .cloned()
            else {
                return Err(error(
                    4,
                    format!(
                        "Failed to apply update due to missing _id: {}",
                        quoted_entry(entry)
                    ),
                ));
            };
            if !exists {
                return Err(missing_ns());
            }
            let diff = if o.get("$v").and_then(crate::util::as_i64) == Some(2) {
                Some(
                    o.get_document("diff")
                        .map_err(|_| error(4770501, "Missing 'diff' field in $v:2 update"))?,
                )
            } else if o.contains_key("_id") {
                None
            } else {
                let got = match o.get("$v") {
                    Some(v) => format!("$v: {}", render(v)),
                    None => "EOO".to_string(),
                };
                return Err(error(
                    4772600,
                    format!("Expected _id field or $v:2, but got: {got}"),
                ));
            };
            let current = find_by_id(storage.as_ref(), db, coll, &id)?;
            let exists_doc = current.is_some();
            let upsert = entry.get("b").is_some_and(crate::util::truthy);
            let post = match (diff, current) {
                (Some(diff), Some(pre)) => apply_diff(pre, diff)?,
                (Some(diff), None) if upsert => apply_diff(doc! {"_id": id.clone()}, diff)?,
                (None, Some(_)) | (None, None) if upsert || o.get("_id") == Some(&id) => {
                    if o.get("_id") != Some(&id) {
                        return Err(error(13596, "in Collection::updateDocument _id mismatch"));
                    }
                    o.clone()
                }
                (None, Some(_)) => {
                    return Err(error(13596, "in Collection::updateDocument _id mismatch"))
                }
                _ => return Ok(Outcome::NotApplied),
            };
            if post.get("_id") != Some(&id) {
                return Err(error(13596, "in Collection::updateDocument _id mismatch"));
            }
            // A diff applied to a stored document is written as the fields it
            // changes, so the write is logged as an update of those fields.
            if let (Some(diff), true) = (diff, exists_doc) {
                if let Some(v) = &validator {
                    let _opaque = secantus_core::deadline::opaque();
                    if !secantus_core::query::matches(&post, v, &Document::new(), None)
                        .unwrap_or(true)
                    {
                        return Ok(Outcome::NotApplied);
                    }
                }
                let update = diff_as_update(diff, &post);
                if !update.is_empty() {
                    storage
                        .update_matching(db, coll, &doc! {"_id": id}, &update, false, false)
                        .map_err(crate::util::command_error)?;
                }
                return Ok(Outcome::Applied);
            }
            write_document(storage.as_ref(), db, coll, &id, post, validator.as_ref())
        }
        _ => {
            let Some(id) = o.get("_id").cloned() else {
                return Err(error(
                    4,
                    format!(
                        "Failed to apply delete due to missing _id: {}",
                        quoted_entry(entry)
                    ),
                ));
            };
            if exists {
                storage
                    .delete_matching(db, coll, &doc! {"_id": id}, 1)
                    .map_err(crate::util::command_error)?;
            }
            Ok(Outcome::Applied)
        }
    }
}

fn is_timeseries(storage: &dyn crate::Storage, db: &str, coll: &str) -> bool {
    storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
}

/// The collection's validator, when it is enforced on inserts.
fn collection_validator(storage: &dyn crate::Storage, db: &str, coll: &str) -> Option<Document> {
    let opts = storage.get_collection_options(db, coll).ok()?;
    let action = opts.get_str("validationAction").unwrap_or("error");
    let level = opts.get_str("validationLevel").unwrap_or("strict");
    if level == "off" || action == "warn" || action == "off" {
        return None;
    }
    opts.get("validator").and_then(Bson::as_document).cloned()
}

fn find_by_id(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    id: &Bson,
) -> Result<Option<Document>, CommandError> {
    let rows = storage
        .find(db, coll, &doc! {"_id": id.clone()}, None, None)
        .map_err(crate::util::command_error)?;
    rows.first()
        .map(|raw| crate::bsondoc::from_bytes(raw))
        .transpose()
        .map_err(|e| error(1, e.to_string()))
}

/// Write `doc` as the document with `id`: a replacement when one exists, an
/// insert when none does.
fn write_document(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    id: &Bson,
    doc: Document,
    validator: Option<&Document>,
) -> Result<Outcome, CommandError> {
    if let Some(v) = validator {
        // A validator no interrupt reaches (RM-313).
        let _opaque = secantus_core::deadline::opaque();
        if !secantus_core::query::matches(&doc, v, &Document::new(), None).unwrap_or(true) {
            return Ok(Outcome::NotApplied);
        }
    }
    let filter = doc! {"_id": id.clone()};
    let outcome = storage
        .update_matching(db, coll, &filter, &doc, false, false)
        .map_err(crate::util::command_error)?;
    if outcome.matched > 0 {
        return Ok(Outcome::Applied);
    }
    let bytes = bson::to_vec(&doc).map_err(|e| error(1, e.to_string()))?;
    let (_, errors) = storage
        .insert(db, coll, vec![bytes], true)
        .map_err(crate::util::command_error)?;
    if let Some(e) = errors.first() {
        let code = e.get_i32("code").unwrap_or(1);
        let mut err = error(code, e.get_str("errmsg").unwrap_or_default().to_string());
        let mut extra = Document::new();
        for k in ["keyPattern", "keyValue"] {
            if let Some(v) = e.get(k) {
                extra.insert(k, v.clone());
            }
        }
        if !extra.is_empty() {
            err = err.with_extra(extra);
        }
        return Err(err);
    }
    Ok(Outcome::Applied)
}

/// Apply a `c` entry through the command it names. A command already true of
/// the catalog (a collection that exists, an index or collection that does
/// not) succeeds, as oplog application does.
fn apply_command(
    db: &str,
    o: &Document,
    ctx: &mut CommandContext,
) -> Result<Outcome, CommandError> {
    let name = o.keys().next().map(String::as_str).unwrap_or("");
    if matches!(
        name,
        "createIndexes" | "startIndexBuild" | "commitIndexBuild" | "abortIndexBuild"
    ) {
        return Err(error(
            115,
            format!("The {name} operation is not supported in applyOps mode"),
        ));
    }
    if matches!(name, "commitTransaction" | "abortTransaction") {
        return Err(error(
            50987,
            format!("{name} is only used internally by secondaries."),
        ));
    }
    if name == "applyOps" {
        let reply = apply_ops(o, ctx)?;
        if reply.get("ok").and_then(crate::util::as_i64) == Some(1) {
            return Ok(Outcome::Applied);
        }
        let code = reply.get_i32("code").unwrap_or(8);
        return Err(error(
            code,
            reply.get_str("errmsg").unwrap_or_default().to_string(),
        ));
    }
    if !APPLIED_COMMANDS.contains(&name) {
        return Err(error(
            115,
            format!("applyOps of a '{name}' command is not supported"),
        ));
    }
    let (command_db, command) = match name {
        "renameCollection" => ("admin", rename_moving_target_aside(o, ctx)?),
        "deleteIndex" | "deleteIndexes" => {
            let mut c = doc! {"dropIndexes": o.get(name).cloned().unwrap_or(Bson::Null)};
            c.extend(o.iter().skip(1).map(|(k, v)| (k.clone(), v.clone())));
            (db, c)
        }
        _ => (db, o.clone()),
    };
    let Some(handler) = crate::lookup(command.keys().next().map(String::as_str).unwrap_or(""))
    else {
        return Err(error(
            115,
            format!("applyOps of a '{name}' command is not supported"),
        ));
    };
    let saved = std::mem::replace(&mut ctx.db_name, command_db.to_string());
    let result = handler(&command, ctx);
    ctx.db_name = saved;
    match result {
        Ok(reply) if reply.get("ok").and_then(crate::util::as_i64) == Some(0) => {
            let code = reply.get_i32("code").unwrap_or(1);
            already_true(name, code)
                .then_some(Outcome::Applied)
                .ok_or_else(|| {
                    error(
                        code,
                        reply.get_str("errmsg").unwrap_or_default().to_string(),
                    )
                })
        }
        Ok(_) => Ok(Outcome::Applied),
        Err(e) if already_true(name, e.code) => Ok(Outcome::Applied),
        Err(e) => Err(e),
    }
}

/// A rename onto a collection that exists moves that collection aside to a
/// temporary name first, with or without `dropTarget`, as oplog application
/// does (mongod 8.2.11 keeps it as `tmpXXXXX.rename`). Returns the rename to
/// run, without `dropTarget`.
fn rename_moving_target_aside(
    o: &Document,
    ctx: &mut CommandContext,
) -> Result<Document, CommandError> {
    let mut rename = o.clone();
    rename.remove("dropTarget");
    let Ok(to) = o.get_str("to") else {
        return Ok(rename);
    };
    let (to_db, to_coll) = split_ns(to);
    let storage = ctx.storage()?;
    if !storage.collection_exists(to_db, to_coll).unwrap_or(false) {
        return Ok(rename);
    }
    let suffix: String = bson::oid::ObjectId::new()
        .to_hex()
        .chars()
        .rev()
        .take(5)
        .collect();
    let aside = doc! {
        "renameCollection": to,
        "to": format!("{to_db}.tmp{suffix}.rename"),
    };
    let handler = crate::lookup("renameCollection")
        .ok_or_else(|| error(115, "renameCollection is not a command here"))?;
    let saved = std::mem::replace(&mut ctx.db_name, "admin".to_string());
    let result = handler(&aside, ctx);
    ctx.db_name = saved;
    match result {
        Ok(reply) if reply.get("ok").and_then(crate::util::as_i64) == Some(0) => Err(error(
            reply.get_i32("code").unwrap_or(1),
            reply.get_str("errmsg").unwrap_or_default().to_string(),
        )),
        Ok(_) => Ok(rename),
        Err(e) => Err(e),
    }
}

/// Whether a command's error means its effect already holds.
fn already_true(name: &str, code: i32) -> bool {
    match name {
        "create" => code == 48,
        "drop" | "collMod" => code == 26,
        "dropIndexes" | "deleteIndex" | "deleteIndexes" => matches!(code, 26 | 27),
        _ => false,
    }
}

/// A document diff's top-level changes as update operators: `$set` for an
/// updated, inserted or sub-diffed field (its value in `post`), `$unset` for
/// a deleted one.
fn diff_as_update(diff: &Document, post: &Document) -> Document {
    let mut set = Document::new();
    let mut unset = Document::new();
    for (section, body) in diff {
        let Bson::Document(fields) = body else {
            continue;
        };
        match section.as_str() {
            "d" => {
                for k in fields.keys() {
                    unset.insert(k.clone(), "");
                }
            }
            "u" | "i" => {
                for (k, v) in fields {
                    set.insert(k.clone(), v.clone());
                }
            }
            s if s.len() > 1 && s.starts_with('s') => {
                if let Some(v) = post.get(&s[1..]) {
                    set.insert(&s[1..], v.clone());
                }
            }
            _ => {}
        }
    }
    let mut update = Document::new();
    if !set.is_empty() {
        update.insert("$set", set);
    }
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }
    update
}

/// Apply mongod's `$v: 2` document diff to `doc`.
pub(crate) fn apply_diff(doc: Document, diff: &Document) -> Result<Document, CommandError> {
    let mut deletes: Vec<&str> = Vec::new();
    let mut updates: Vec<(&str, &Bson)> = Vec::new();
    let mut inserts: Vec<(&str, &Bson)> = Vec::new();
    let mut subdiffs: Vec<(&str, &Document)> = Vec::new();
    for (section, body) in diff {
        let first = section.chars().next().unwrap_or(' ');
        match first {
            'd' | 'u' | 'i' if section.len() == 1 => {
                let Bson::Document(fields) = body else {
                    return Err(error(
                        4770507,
                        format!("Expected sub-diff to be an object, got {}", type_name(body)),
                    ));
                };
                for (k, v) in fields {
                    match first {
                        'd' => deletes.push(k),
                        'u' => updates.push((k, v)),
                        _ => inserts.push((k, v)),
                    }
                }
            }
            's' if section.len() > 1 => {
                let Bson::Document(sub) = body else {
                    return Err(error(
                        4770507,
                        format!("Expected sub-diff to be an object, got {}", type_name(body)),
                    ));
                };
                subdiffs.push((&section[1..], sub));
            }
            c => {
                return Err(error(
                    4770503,
                    format!("Unexpected section: {c} in document diff"),
                ))
            }
        }
    }
    let mut out = Document::new();
    for (k, v) in doc {
        if deletes.contains(&k.as_str()) || inserts.iter().any(|(f, _)| *f == k) {
            continue;
        }
        if let Some((_, nv)) = updates.iter().find(|(f, _)| *f == k) {
            out.insert(k, (*nv).clone());
        } else if let Some((_, sub)) = subdiffs.iter().find(|(f, _)| *f == k) {
            let applied = apply_subdiff(v, sub)?;
            out.insert(k, applied);
        } else {
            out.insert(k, v);
        }
    }
    for (k, v) in updates.iter().chain(inserts.iter()) {
        if !out.contains_key(*k) {
            out.insert(*k, (*v).clone());
        }
    }
    Ok(out)
}

/// A sub-diff of one field: an array diff (`a: true` first) or a document
/// diff. In an array diff a new length `l` may only follow `a`, and every
/// other entry is `u<index>` or `s<index>` (mongod 8.2.11's rules and codes).
fn apply_subdiff(value: Bson, sub: &Document) -> Result<Bson, CommandError> {
    let is_array_diff = sub
        .iter()
        .next()
        .is_some_and(|(k, v)| k == "a" && crate::util::truthy(v));
    if !is_array_diff {
        return match value {
            Bson::Document(d) => Ok(Bson::Document(apply_diff(d, sub)?)),
            _ => Err(error(
                4770508,
                "Cannot apply a document diff to a non-object value",
            )),
        };
    }
    let Bson::Array(mut items) = value else {
        return Err(error(
            4770508,
            "Cannot apply an array diff to a non-array value",
        ));
    };
    let mut entries = sub.iter().skip(1).peekable();
    let mut resized = None;
    if let Some((_, len)) = entries.next_if(|(k, _)| k.as_str() == "l") {
        let len = crate::util::as_i64(len).unwrap_or(0).max(0) as usize;
        items.truncate(len);
        resized = Some(len);
    }
    for (k, v) in entries {
        if k.chars().count() < 2 {
            return Err(error(
                4770521,
                format!("expected field name to be at least two characters long, but found: {k}"),
            ));
        }
        let (kind, rest) = k.split_at(1);
        let idx: usize = rest
            .parse()
            .map_err(|_| error(4770503, format!("Unexpected section: {k} in array diff")))?;
        if !matches!(kind, "u" | "s") {
            return Err(error(
                4770502,
                format!(
                    "Expected either 'u' (update) or 's' (sub diff) at index {idx} but got {k}: {}",
                    render(v)
                ),
            ));
        }
        if resized.is_some_and(|len| idx >= len) {
            return Err(error(
                12495901,
                "array diff update index exceeds resize value",
            ));
        }
        while items.len() <= idx {
            items.push(Bson::Null);
        }
        if kind == "u" {
            items[idx] = v.clone();
        } else {
            let Bson::Document(d) = v else {
                return Err(error(4770507, "Expected sub-diff to be an object"));
            };
            let current = std::mem::replace(&mut items[idx], Bson::Null);
            items[idx] = apply_subdiff(current, d)?;
        }
    }
    Ok(Bson::Array(items))
}

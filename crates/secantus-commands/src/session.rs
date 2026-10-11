//! The session and transaction fields any command may carry -- `lsid`,
//! `txnNumber`, `autocommit`, `startTransaction` -- and what they make of the
//! command: a statement of a multi-document transaction, a retryable write,
//! or an ordinary command that belongs to a session.
//!
//! Every rule here was measured against mongod 8.2.11 with
//! `tools/probes/transactions.py`. Three of them are easy to get wrong:
//!
//! * A command a transaction does not allow at all (`count`, `ping`, ...) is
//!   refused BEFORE the transaction is touched: as a first statement it starts
//!   nothing, and later it leaves the transaction open. A statement that
//!   fails while RUNNING (a `$out` stage, a duplicate key) aborts it.
//! * A retryable write is remembered per STATEMENT, and only for a statement
//!   that wrote. A retry answers for the statements already applied
//!   (`retriedStmtIds`) and runs the rest.
//! * A cursor belongs to the session, and the transaction, that opened it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bson::{doc, Bson, Document};

use crate::cursors::CursorOwner;
use crate::transactions::{self, StmtRecord, Transaction, TransactionRegistry};
use crate::{CommandContext, CommandError, Handler};

/// A command's session fields, checked.
pub(crate) struct Envelope {
    pub lsid: Option<Vec<u8>>,
    pub txn_number: Option<i64>,
    /// `autocommit: false`: a statement of a multi-document transaction.
    pub in_txn: bool,
    pub start: bool,
}

/// The name mongod's parser gives a command's own fields in a type error:
/// the command name, except for `find`.
fn struct_name(command: &str) -> &str {
    if command == "find" {
        "FindCommandRequest"
    } else {
        command
    }
}

fn wrong_type(path: &str, value: &Bson, expected: &str) -> CommandError {
    CommandError::new(
        14,
        "TypeMismatch",
        format!(
            "BSON field '{path}' is the wrong type '{}', expected type '{expected}'",
            secantus_core::query::bson_type_name(value)
        ),
    )
}

fn invalid_options(message: impl Into<String>) -> CommandError {
    CommandError::new(72, "InvalidOptions", message)
}

fn not_in_transaction(message: impl Into<String>) -> CommandError {
    CommandError::new(263, "OperationNotSupportedInTransaction", message)
}

fn binary_subtype_name(subtype: bson::spec::BinarySubtype) -> &'static str {
    use bson::spec::BinarySubtype as B;
    match subtype {
        B::Generic => "general",
        B::Function => "function",
        B::BinaryOld => "byte(deprecated)",
        B::UuidOld => "UUID(deprecated)",
        B::Uuid => "UUID",
        B::Md5 => "MD5",
        B::Encrypted => "encrypt",
        B::Column => "column",
        B::Sensitive => "sensitive",
        _ => "Custom",
    }
}

/// The 16 bytes of a session id document `{id: UUID}`, whose fields are named
/// under `path` in an error.
pub(crate) fn session_id(lsid: &Document, path: &str) -> Result<Vec<u8>, CommandError> {
    let id = match lsid.get("id") {
        None => {
            return Err(CommandError::new(
                40414,
                "IDLFailedToParse",
                format!("BSON field '{path}.id' is missing but a required field"),
            ))
        }
        Some(Bson::Binary(b)) => b,
        Some(other) => return Err(wrong_type(&format!("{path}.id"), other, "binData")),
    };
    if id.subtype != bson::spec::BinarySubtype::Uuid {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            format!(
                "BSON field '{path}.id' is the wrong binData type '{}', expected type 'UUID'",
                binary_subtype_name(id.subtype)
            ),
        ));
    }
    if id.bytes.len() != 16 {
        return Err(CommandError::new(
            207,
            "InvalidUUID",
            "uuid must be a 16-byte binary field with UUID (4) subtype",
        ));
    }
    for key in lsid.keys() {
        if !matches!(key.as_str(), "id" | "uid" | "txnNumber" | "txnUUID") {
            return Err(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("BSON field '{path}.{key}' is an unknown field."),
            ));
        }
    }
    if lsid.contains_key("txnNumber") && !lsid.contains_key("txnUUID") {
        return Err(invalid_options(
            "Cannot specify txnNumber in lsid without specifying txnUUID",
        ));
    }
    Ok(id.bytes.clone())
}

fn bool_field(doc: &Document, field: &str, command: &str) -> Result<Option<bool>, CommandError> {
    match doc.get(field) {
        None | Some(Bson::Null) => Ok(None),
        Some(Bson::Boolean(b)) => Ok(Some(*b)),
        Some(other) => Err(wrong_type(
            &format!("{}.{field}", struct_name(command)),
            other,
            "bool",
        )),
    }
}

/// Read and check a command's session fields.
pub(crate) fn parse(name: &str, doc: &Document) -> Result<Envelope, CommandError> {
    let owner = struct_name(name);
    let lsid = match doc.get("lsid") {
        None | Some(Bson::Null) => None,
        Some(Bson::Document(d)) => Some(session_id(d, &format!("{owner}.lsid"))?),
        Some(other) => return Err(wrong_type(&format!("{owner}.lsid"), other, "object")),
    };
    let txn_number = match doc.get("txnNumber") {
        None | Some(Bson::Null) => None,
        Some(Bson::Int64(n)) => Some(*n),
        Some(other) => return Err(wrong_type(&format!("{owner}.txnNumber"), other, "long")),
    };
    let autocommit = bool_field(doc, "autocommit", name)?;
    let start = bool_field(doc, "startTransaction", name)?;

    if txn_number.is_some() && lsid.is_none() {
        return Err(invalid_options(
            "Transaction number requires a session ID to also be specified",
        ));
    }
    if txn_number.is_some_and(|n| n < 0) {
        return Err(invalid_options("Transaction number cannot be negative"));
    }
    if autocommit.is_some() && txn_number.is_none() {
        return Err(invalid_options(
            "'autocommit' field requires a transaction number to also be specified",
        ));
    }
    if autocommit == Some(true) {
        return Err(invalid_options(
            "Specifying autocommit=true is not allowed.",
        ));
    }
    if start.is_some() && autocommit.is_none() {
        return Err(invalid_options(
            "'startTransaction' field requires 'autocommit' field to also be specified",
        ));
    }
    if start == Some(false) {
        return Err(invalid_options(
            "Specifying startTransaction=false is not allowed.",
        ));
    }
    Ok(Envelope {
        lsid,
        txn_number,
        in_txn: autocommit == Some(false),
        start: start == Some(true),
    })
}

fn is_ending(name: &str) -> bool {
    name == "commitTransaction" || name == "abortTransaction"
}

fn is_retryable_write(name: &str) -> bool {
    matches!(
        name,
        "insert" | "update" | "delete" | "findAndModify" | "findandmodify" | "bulkWrite"
    )
}

/// Run `handler` for `name` as its session fields direct.
pub(crate) fn run(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
) -> Document {
    let env = match parse(name, doc) {
        Ok(env) => env,
        Err(e) => return e.into_reply(),
    };
    let owner = env.lsid.clone().map(|lsid| CursorOwner {
        lsid,
        txn_number: env.txn_number.filter(|_| env.in_txn),
    });
    crate::cursors::with_owner(owner, || run_checked(name, handler, doc, ctx, &env))
}

fn run_checked(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    env: &Envelope,
) -> Document {
    if is_ending(name) {
        if let Some(e) = ending_problem(name, doc, ctx, env) {
            return e.into_reply();
        }
        return crate::run_handler(handler, doc, ctx);
    }
    let (Some(lsid), Some(txn_number)) = (&env.lsid, env.txn_number) else {
        if name == "getMore" {
            if let Some(e) = cursor_problem(doc, ctx, env) {
                return e.into_reply();
            }
        }
        return crate::run_handler(handler, doc, ctx);
    };
    if !env.in_txn {
        if matches!(name, "hello" | "isMaster" | "ismaster" | "ping") {
            return CommandError::new(
                50889,
                "Location50889",
                "It is illegal to provide a txnNumber for this command",
            )
            .into_reply();
        }
        if !is_retryable_write(name) {
            return not_a_retryable_write(name).into_reply();
        }
        let Some(registry) = ctx.transactions.clone() else {
            return crate::run_handler(handler, doc, ctx);
        };
        return retryable_write(name, handler, doc, ctx, &registry, lsid, txn_number);
    }
    // Killing sessions is no statement of the transaction it is sent in: it
    // runs, and may well abort that very transaction.
    if matches!(
        name,
        "killSessions" | "killAllSessions" | "killAllSessionsByPattern"
    ) {
        return crate::run_handler(handler, doc, ctx);
    }
    if let Some(e) = statement_refusal(name, doc, ctx, env) {
        return e.into_reply();
    }
    let Some(registry) = ctx.transactions.clone() else {
        return crate::run_handler(handler, doc, ctx);
    };
    let txn = match registry.for_statement(lsid, txn_number, env.start) {
        Ok(t) => t,
        Err(reply) => return reply,
    };
    statement(name, handler, doc, ctx, env, &registry, &txn)
}

fn not_a_retryable_write(name: &str) -> CommandError {
    CommandError::new(
        50768,
        "NotARetryableWriteCommand",
        format!(
            "txnNumber may only be provided for multi-document transactions and retryable \
             write commands. autocommit:false was not provided, and {name} is not a retryable \
             write command."
        ),
    )
}

/// Why a `commitTransaction` / `abortTransaction` cannot run as sent.
fn ending_problem(
    name: &str,
    doc: &Document,
    ctx: &CommandContext,
    env: &Envelope,
) -> Option<CommandError> {
    if env.txn_number.is_none() {
        return Some(CommandError::new(
            125,
            "CommandFailed",
            format!("{name} must be run within a transaction"),
        ));
    }
    if !env.in_txn {
        return Some(not_a_retryable_write(name));
    }
    if ctx.db_name != "admin" {
        return Some(CommandError::new(
            13,
            "Unauthorized",
            format!("{name} may only be run against the admin database."),
        ));
    }
    if env.start {
        let what = if name == "commitTransaction" {
            "a commit"
        } else {
            "an abort"
        };
        return Some(not_in_transaction(format!(
            "Cannot start a transaction with {what}"
        )));
    }
    if name == "commitTransaction" {
        const KNOWN: &[&str] = &[
            "commitTransaction",
            "lsid",
            "txnNumber",
            "txnRetryCounter",
            "autocommit",
            "startTransaction",
            "writeConcern",
            "readConcern",
            "comment",
            "maxTimeMS",
            "recoveryToken",
            "commitTimestamp",
            "apiVersion",
            "apiStrict",
            "apiDeprecationErrors",
        ];
        if let Some(key) = doc
            .keys()
            .find(|k| !k.starts_with('$') && !KNOWN.contains(&k.as_str()))
        {
            return Some(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("BSON field 'commitTransaction.{key}' is an unknown field."),
            ));
        }
    }
    None
}

/// Commands a multi-document transaction may contain.
fn allowed_in_transaction(name: &str) -> bool {
    matches!(
        name,
        "insert"
            | "update"
            | "delete"
            | "findAndModify"
            | "findandmodify"
            | "find"
            | "getMore"
            | "killCursors"
            | "aggregate"
            | "distinct"
            | "bulkWrite"
            | "create"
            | "createIndexes"
    )
}

/// Why a command cannot be a statement of a transaction at all. Checked
/// before the transaction is looked up, so it neither starts nor aborts one.
fn statement_refusal(
    name: &str,
    doc: &Document,
    ctx: &CommandContext,
    env: &Envelope,
) -> Option<CommandError> {
    if !allowed_in_transaction(name) {
        return Some(not_in_transaction(match name {
            "hello" | "isMaster" | "ismaster" | "ping" | "connectionStatus" => {
                "This command is not supported in transactions".to_string()
            }
            "count" => "Cannot run 'count' in a multi-document transaction. Please see \
                        http://dochub.mongodb.org/core/transaction-count for a recommended \
                        alternative."
                .to_string(),
            _ => format!("Cannot run '{name}' in a multi-document transaction."),
        }));
    }
    match ctx.db_name.as_str() {
        "config" => {
            return Some(not_in_transaction(
                "Cannot run command against the config database in a transaction.",
            ))
        }
        "local" => {
            return Some(not_in_transaction(
                "Cannot run command against the 'local' database in a transaction.",
            ))
        }
        _ => {}
    }
    if doc.get_str(name) == Ok("system.views") {
        return Some(not_in_transaction(format!(
            "Cannot run command against the '{}.system.views' collection in a transaction.",
            ctx.db_name
        )));
    }
    if name == "killCursors" && env.start {
        return Some(not_in_transaction(
            "Cannot run killCursors as the first operation in a multi-document transaction.",
        ));
    }
    if doc.get("writeConcern").is_some_and(|w| *w != Bson::Null) {
        return Some(invalid_options(
            "writeConcern is not allowed within a multi-statement transaction",
        ));
    }
    if let Some(Bson::Document(rc)) = doc.get("readConcern") {
        if !env.start {
            if !rc.is_empty() {
                return Some(invalid_options(
                    "Only the first command in a transaction may specify a readConcern",
                ));
            }
        } else if let Some(level) = rc.get("level").and_then(Bson::as_str) {
            if !matches!(level, "local" | "majority" | "snapshot") {
                return Some(invalid_options(
                    "The readConcern level must be either 'local' (default), 'majority' or \
                     'snapshot' in order to run in a transaction",
                ));
            }
        }
    }
    None
}

/// Why a `getMore` may not read its cursor: a cursor is read only by the
/// session, and the transaction, that opened it.
fn cursor_problem(doc: &Document, ctx: &CommandContext, env: &Envelope) -> Option<CommandError> {
    let id = doc.get("getMore").and_then(crate::util::as_i64)?;
    let opened_by = ctx.cursors.as_ref()?.owner(id)?;
    let refuse = |code: i32, rest: String| {
        Some(CommandError::new(
            code,
            format!("Location{code}"),
            format!("Cannot run getMore on cursor {id}, which was {rest}"),
        ))
    };
    let Some(owner) = opened_by else {
        return match &env.lsid {
            Some(lsid) => refuse(
                50736,
                format!(
                    "not created in a session, in session {}",
                    transactions::session_text(lsid)
                ),
            ),
            None => None,
        };
    };
    let created = transactions::session_text(&owner.lsid);
    let Some(lsid) = &env.lsid else {
        return refuse(
            50737,
            format!("created in session {created}, without an lsid"),
        );
    };
    if *lsid != owner.lsid {
        return refuse(
            50738,
            format!(
                "created in session {created}, in session {}",
                transactions::session_text(lsid)
            ),
        );
    }
    let now = env.txn_number.filter(|_| env.in_txn);
    match (owner.txn_number, now) {
        (None, Some(n)) => refuse(
            50739,
            format!("not created in a transaction, in transaction {n}"),
        ),
        (Some(m), None) => refuse(
            50740,
            format!("created in transaction {m}, without a txnNumber"),
        ),
        (Some(m), Some(n)) if m != n => refuse(
            50741,
            format!("created in transaction {m}, in transaction {n}"),
        ),
        _ => None,
    }
}

/// Aggregation stages a transaction refuses, and how mongod words each.
fn stage_refusal(pipeline: &[Bson]) -> Option<CommandError> {
    for stage in pipeline {
        let Some(name) = stage.as_document().and_then(|d| d.keys().next()) else {
            continue;
        };
        let message = match name.as_str() {
            "$out" | "$merge" => format!("{name} cannot be used in a transaction"),
            "$changeStream" | "$listLocalSessions" | "$listSessions" => format!(
                "Operation not permitted in transaction :: caused by :: Aggregation stage \
                 {name} cannot run within a multi-document transaction."
            ),
            "$collStats" | "$indexStats" | "$unionWith" | "$currentOp" | "$planCacheStats" => {
                format!("Stage not supported inside of a multi-document transaction: {name}")
            }
            _ => continue,
        };
        return Some(not_in_transaction(message));
    }
    None
}

/// What a transaction knows of a collection it created: whether it has since
/// put a document in it.
type Created = HashMap<String, bool>;

/// Why a statement that IS allowed in a transaction cannot run in this one.
/// The transaction is open by now, and this aborts it.
fn running_refusal(
    name: &str,
    doc: &Document,
    ctx: &CommandContext,
    env: &Envelope,
    created: &Created,
) -> Option<CommandError> {
    if name == "getMore" {
        return cursor_problem(doc, ctx, env);
    }
    if name == "aggregate" {
        if let Some(Bson::Array(pipeline)) = doc.get("pipeline") {
            return stage_refusal(pipeline);
        }
        return None;
    }
    let coll = doc.get_str(name).ok()?;
    let ns = format!("{}.{coll}", ctx.db_name);
    let storage = ctx.storage.as_ref()?;
    let exists = storage
        .collection_exists(&ctx.db_name, coll)
        .unwrap_or(false);
    let is_write = matches!(
        name,
        "insert" | "update" | "delete" | "findAndModify" | "findandmodify"
    );
    match name {
        "find" => {
            if doc.get("tailable").is_some_and(crate::util::truthy) {
                return Some(invalid_options(
                    "It is illegal to open a tailable cursor in a transaction",
                ));
            }
        }
        "create" => {
            if doc.contains_key("viewOn") {
                return Some(not_in_transaction(
                    "Cannot create a view in a multi-document transaction.",
                ));
            }
            if doc.contains_key("timeseries") {
                return Some(not_in_transaction(
                    "Cannot create a time-series collection in a multi-document transaction.",
                ));
            }
            if exists || created.contains_key(&ns) {
                return Some(CommandError::new(
                    48,
                    "NamespaceExists",
                    format!("Collection {ns} already exists."),
                ));
            }
        }
        "createIndexes" => {
            let have: HashSet<String> = storage
                .list_indexes(&ctx.db_name, coll)
                .unwrap_or_default()
                .iter()
                .filter_map(|ix| ix.get_str("name").ok().map(String::from))
                .collect();
            let all_exist = exists
                && doc.get_array("indexes").is_ok_and(|specs| {
                    specs.iter().all(|spec| {
                        spec.as_document()
                            .and_then(|s| s.get_str("name").ok())
                            .is_some_and(|n| have.contains(n))
                    })
                });
            if exists && !all_exist {
                return Some(not_in_transaction(format!(
                    "Cannot create new indexes on existing collection {ns} in a multi-document \
                     transaction."
                )));
            }
            if created.get(&ns) == Some(&true) {
                return Some(not_in_transaction(format!(
                    "Cannot create new indexes on non-empty collection {ns} in a multi-document \
                     transaction."
                )));
            }
        }
        _ => {}
    }
    if is_write && coll.starts_with("system.") {
        return Some(CommandError::new(
            50791,
            "Location50791",
            format!("Cannot write to system collection {ns} within a transaction."),
        ));
    }
    if name == "insert" && exists {
        let options = storage
            .get_collection_options(&ctx.db_name, coll)
            .unwrap_or_default();
        if options.contains_key("timeseries") {
            return Some(not_in_transaction(format!(
                "time-series insert failed: {ns} :: caused by :: Cannot insert into a \
                 time-series collection in a multi-document transaction: {ns}"
            )));
        }
    }
    None
}

/// Run a statement inside its transaction, which `for_statement` resolved.
fn statement(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    env: &Envelope,
    registry: &TransactionRegistry,
    txn: &Arc<Mutex<Transaction>>,
) -> Document {
    let created = txn
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .created
        .clone();
    if let Some(e) = running_refusal(name, doc, ctx, env, &created) {
        registry.abort_in_progress(txn);
        return e.into_reply();
    }
    if let Some(reply) = crate::capped_write_in_txn(name, doc, ctx) {
        registry.abort_in_progress(txn);
        return reply;
    }
    if let Some(mut reply) = crate::views::refusal(name, doc, ctx) {
        crate::finish_txn_statement(registry, txn, &mut reply);
        return reply;
    }
    let Some(storage) = ctx.storage.clone() else {
        return crate::run_handler(handler, doc, ctx);
    };
    // The namespace this statement may bring into being.
    let target = doc.get_str(name).ok().and_then(|coll| {
        let makes = matches!(
            name,
            "insert" | "update" | "findAndModify" | "findandmodify" | "create" | "createIndexes"
        );
        let exists = storage
            .collection_exists(&ctx.db_name, coll)
            .unwrap_or(true);
        (makes && !exists).then(|| format!("{}.{coll}", ctx.db_name))
    });
    // The first error ends a transaction, so an unordered batch has nothing to
    // carry on to: mongod stops at the first failed statement either way.
    let ordered;
    let doc = if matches!(doc.get("ordered"), Some(Bson::Boolean(false))) {
        let mut copy = doc.clone();
        copy.insert("ordered", true);
        ordered = copy;
        &ordered
    } else {
        doc
    };
    // Create the WT transaction lazily at the first statement (the snapshot
    // pins here).
    {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        if t.handle.is_none() {
            match storage.begin_user_transaction() {
                Ok(h) => t.handle = Some(h),
                Err(e) => {
                    return CommandError::new(1, "InternalError", format!("{e:?}")).into_reply()
                }
            }
        }
    }
    let mut result = {
        let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
        let handle = t.handle.as_mut().expect("handle created above").as_mut();
        match storage.run_in_user_transaction(handle, &mut || crate::run_handler(handler, doc, ctx))
        {
            Ok(r) => r,
            Err(e) => CommandError::new(1, "InternalError", format!("{e:?}")).into_reply(),
        }
    };
    let code = result.get_i32("code").unwrap_or(0);
    let unparsed = matches!(code, 16 | 40414 | 40415)
        || result
            .get_str("errmsg")
            .is_ok_and(|m| m.starts_with("BSON field '") || m.starts_with("Expected field "));
    if !ok(&result) && unparsed {
        // The command did not parse, so mongod never took it into the
        // transaction: one it would have started does not exist, and one
        // already open stays open.
        if env.start {
            if let Some(lsid) = &env.lsid {
                registry.forget_started(lsid, txn);
            }
        }
        return result;
    }
    if name == "insert" && code == 112 {
        // An insert conflicts on an index, and mongod names its own table for
        // that index as the cause, or nothing.
        result.insert(
            "errmsg",
            "Caused by ::  :: Please retry your operation or multi-document transaction.",
        );
    }
    crate::finish_txn_statement(registry, txn, &mut result);
    if let Some(ns) = target {
        if result.get_f64("ok").unwrap_or(0.0) == 1.0 && !result.contains_key("writeErrors") {
            let wrote = match name {
                "insert" => crate::util::as_i64_field(&result, "n") > 0,
                "update" => result.contains_key("upserted"),
                "create" | "createIndexes" => false,
                _ => result
                    .get_document("lastErrorObject")
                    .is_ok_and(|l| l.contains_key("upserted")),
            };
            if wrote || matches!(name, "create" | "createIndexes") {
                let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
                let entry = t.created.entry(ns).or_insert(false);
                *entry = *entry || wrote;
            }
            // mongod reports the collection as made by this command even when
            // an earlier `create` of the same transaction made it.
            if name == "createIndexes" && result.contains_key("createdCollectionAutomatically") {
                result.insert("createdCollectionAutomatically", true);
            }
        }
    } else if matches!(
        name,
        "insert" | "update" | "findAndModify" | "findandmodify"
    ) {
        if let Ok(coll) = doc.get_str(name) {
            let ns = format!("{}.{coll}", ctx.db_name);
            let mut t = txn.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(non_empty) = t.created.get_mut(&ns) {
                *non_empty = true;
            }
        }
    }
    result
}

// ------------------------------------------------------------ retryable writes

fn ok(reply: &Document) -> bool {
    reply.get_f64("ok").unwrap_or(0.0) == 1.0
}

/// The statement id of each of a write command's `count` statements:
/// `stmtIds`, or `stmtId` (default 0) counting up.
fn statement_ids(name: &str, doc: &Document, count: usize) -> Result<Vec<i32>, CommandError> {
    let first = match doc.get("stmtId") {
        None | Some(Bson::Null) => None,
        Some(Bson::Int32(n)) => Some(*n),
        Some(other) => return Err(wrong_type(&format!("{name}.stmtId"), other, "int")),
    };
    let listed = match doc.get("stmtIds") {
        None | Some(Bson::Null) => None,
        Some(Bson::Array(ids)) => Some(
            ids.iter()
                .map(|v| crate::util::as_i64(v).unwrap_or(0) as i32)
                .collect::<Vec<i32>>(),
        ),
        Some(other) => return Err(wrong_type(&format!("{name}.stmtIds"), other, "array")),
    };
    match (first, listed) {
        (Some(_), Some(_)) => Err(invalid_options(
            "May not specify both stmtId and stmtIds in write command.",
        )),
        (_, Some(ids)) if ids.len() != count => Err(CommandError::new(
            16,
            "InvalidLength",
            format!(
                "Number of statement ids must match the number of batch entries. Got {} \
                 statement ids but {count} operations.",
                ids.len()
            ),
        )),
        (_, Some(ids)) => Ok(ids),
        (first, None) => {
            let first = first.unwrap_or(0);
            Ok((0..count).map(|i| first.wrapping_add(i as i32)).collect())
        }
    }
}

fn indices_of(reply: &Document, field: &str) -> HashSet<usize> {
    reply
        .get_array(field)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.as_document()?.get("index").and_then(crate::util::as_i64))
                .map(|i| i as usize)
                .collect()
        })
        .unwrap_or_default()
}

/// Rewrite the `index` of each entry of `reply[field]` through `original`.
fn remap_indices(reply: &mut Document, field: &str, original: &[usize]) {
    if let Ok(entries) = reply.get_array_mut(field) {
        for entry in entries.iter_mut() {
            if let Bson::Document(d) = entry {
                if let Some(i) = d.get("index").and_then(crate::util::as_i64) {
                    if let Some(orig) = original.get(i as usize) {
                        d.insert("index", *orig as i32);
                    }
                }
            }
        }
    }
}

fn sort_by_index(reply: &mut Document, field: &str) {
    if let Ok(entries) = reply.get_array_mut(field) {
        entries.sort_by_key(|e| {
            e.as_document()
                .and_then(|d| d.get("index"))
                .and_then(crate::util::as_i64)
                .unwrap_or(0)
        });
    }
}

fn add_count(reply: &mut Document, field: &str, by: i32) {
    let now = crate::util::as_i64_field(reply, field) as i32;
    reply.insert(field, now + by);
}

fn push_entry(reply: &mut Document, field: &str, entry: Document) {
    match reply.get_array_mut(field) {
        Ok(entries) => entries.push(Bson::Document(entry)),
        Err(_) => {
            reply.insert(field, vec![Bson::Document(entry)]);
        }
    }
}

fn retryable_write(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    registry: &TransactionRegistry,
    lsid: &[u8],
    txn_number: i64,
) -> Document {
    // The statement ids are part of the command: a malformed set is refused
    // before the session's number moves.
    let count = match name {
        "insert" => match (&ctx.raw_insert_documents, doc.get_array("documents")) {
            (Some(raw), _) => raw.len(),
            (None, Ok(docs)) => docs.len(),
            _ => 0,
        },
        "update" => doc.get_array("updates").map_or(0, Vec::len),
        "delete" => doc.get_array("deletes").map_or(0, Vec::len),
        _ => 1,
    };
    let ids = match statement_ids(name, doc, count) {
        Ok(ids) => ids,
        Err(e) => return e.into_reply(),
    };
    let done = match registry.begin_retryable(lsid, txn_number) {
        Ok(done) => done,
        Err(reply) => return reply,
    };
    let (reply, records) = match name {
        "insert" => retry_insert(handler, doc, ctx, &ids, &done),
        "update" | "delete" => retry_batch(name, handler, doc, ctx, &ids, &done),
        "findAndModify" | "findandmodify" => retry_find_and_modify(handler, doc, ctx, &ids, &done),
        // `bulkWrite` takes the session's number and is not replayed.
        _ => (crate::run_handler(handler, doc, ctx), Vec::new()),
    };
    registry.record_statements(lsid, txn_number, records);
    reply
}

type Ran = (Document, Vec<(i32, StmtRecord)>);

fn with_retried(mut reply: Document, retried: Vec<i32>) -> Document {
    if !retried.is_empty() {
        reply.insert("retriedStmtIds", retried);
    }
    reply
}

fn retry_insert(
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    ids: &[i32],
    done: &HashMap<i32, StmtRecord>,
) -> Ran {
    let fresh: Vec<usize> = (0..ids.len())
        .filter(|i| !done.contains_key(&ids[*i]))
        .collect();
    let retried: Vec<i32> = ids
        .iter()
        .copied()
        .filter(|id| done.contains_key(id))
        .collect();
    let id_of = |index: usize| -> Option<Bson> {
        doc.get_array("documents")
            .ok()?
            .get(index)?
            .as_document()?
            .get("_id")
            .cloned()
    };
    let mut reply = if retried.is_empty() {
        crate::run_handler(handler, doc, ctx)
    } else if fresh.is_empty() {
        ctx.raw_insert_documents = None;
        doc! { "n": 0_i32, "ok": 1.0 }
    } else {
        let mut reduced = doc.clone();
        if let Some(raw) = ctx.raw_insert_documents.take() {
            ctx.raw_insert_documents = Some(fresh.iter().map(|i| raw[*i].clone()).collect());
        } else if let Ok(docs) = doc.get_array("documents") {
            reduced.insert(
                "documents",
                fresh
                    .iter()
                    .map(|i| docs[*i].clone())
                    .collect::<Vec<Bson>>(),
            );
        }
        reduced.remove("stmtIds");
        reduced.remove("stmtId");
        crate::run_handler(handler, &reduced, ctx)
    };
    if !ok(&reply) {
        return (reply, Vec::new());
    }
    // Which of the fresh statements were applied: in an ordered batch those
    // before the first failure, otherwise every one that did not fail.
    let failed = indices_of(&reply, "writeErrors");
    let ordered = !matches!(doc.get("ordered"), Some(Bson::Boolean(false)));
    let stop = failed.iter().min().copied();
    let records: Vec<(i32, StmtRecord)> = fresh
        .iter()
        .enumerate()
        .filter(|(position, _)| match (ordered, stop) {
            (true, Some(stop)) => *position < stop,
            _ => !failed.contains(position),
        })
        .map(|(_, index)| (ids[*index], StmtRecord::Insert { id: id_of(*index) }))
        .collect();
    if !retried.is_empty() {
        remap_indices(&mut reply, "writeErrors", &fresh);
        add_count(&mut reply, "n", retried.len() as i32);
    }
    (with_retried(reply, retried), records)
}

fn retry_batch(
    name: &str,
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    ids: &[i32],
    done: &HashMap<i32, StmtRecord>,
) -> Ran {
    let key = if name == "update" {
        "updates"
    } else {
        "deletes"
    };
    let Ok(statements) = doc.get_array(key) else {
        return (crate::run_handler(handler, doc, ctx), Vec::new());
    };
    let ordered = !matches!(doc.get("ordered"), Some(Bson::Boolean(false)));
    // A statement that may touch many documents cannot be retried safely, and
    // mongod refuses it where it stands in the batch.
    let many = |statement: &Bson| -> bool {
        let Some(s) = statement.as_document() else {
            return false;
        };
        if name == "update" {
            s.get("multi").is_some_and(crate::util::truthy)
        } else {
            s.get("limit").and_then(crate::util::as_i64) == Some(0)
        }
    };
    let mut fresh: Vec<usize> = Vec::new();
    let mut retried: Vec<usize> = Vec::new();
    let mut refused: Vec<usize> = Vec::new();
    for (index, statement) in statements.iter().enumerate() {
        if done.contains_key(&ids[index]) {
            retried.push(index);
        } else if many(statement) {
            refused.push(index);
            if ordered {
                break;
            }
        } else {
            fresh.push(index);
        }
    }
    let direct = fresh.len() == statements.len();
    let (mut reply, noted) = transactions::collect_statements(|| {
        if direct {
            crate::run_handler(handler, doc, ctx)
        } else if fresh.is_empty() {
            let mut empty = doc! { "n": 0_i32 };
            if name == "update" {
                empty.insert("nModified", 0_i32);
            }
            empty.insert("ok", 1.0);
            empty
        } else {
            let mut reduced = doc.clone();
            reduced.insert(
                key,
                fresh
                    .iter()
                    .map(|i| statements[*i].clone())
                    .collect::<Vec<Bson>>(),
            );
            reduced.remove("stmtIds");
            reduced.remove("stmtId");
            crate::run_handler(handler, &reduced, ctx)
        }
    });
    if !ok(&reply) {
        return (reply, Vec::new());
    }
    let records: Vec<(i32, StmtRecord)> = noted
        .into_iter()
        .filter_map(|(position, record)| Some((ids[*fresh.get(position)?], record)))
        .collect();
    if direct {
        return (reply, records);
    }
    remap_indices(&mut reply, "writeErrors", &fresh);
    remap_indices(&mut reply, "upserted", &fresh);
    let stopped = ordered && reply.contains_key("writeErrors");
    if !stopped {
        for index in refused {
            let what = if name == "update" {
                "multi=true"
            } else {
                "limit=0"
            };
            push_entry(
                &mut reply,
                "writeErrors",
                doc! {
                    "index": index as i32,
                    "code": 72_i32,
                    "errmsg": format!("Cannot use (or request) retryable writes with {what}"),
                },
            );
        }
    }
    for index in &retried {
        add_count(&mut reply, "n", 1);
        if name != "update" {
            continue;
        }
        // What an update answers for a statement already applied depends on
        // what that statement was: an insert reads as an upsert.
        let upserted = match &done[&ids[*index]] {
            StmtRecord::Insert { id } => Some(id.clone().unwrap_or(Bson::Null)),
            StmtRecord::Update { upserted } => upserted.clone(),
            _ => None,
        };
        match upserted {
            Some(id) => push_entry(
                &mut reply,
                "upserted",
                doc! { "index": *index as i32, "_id": id },
            ),
            None if !matches!(done[&ids[*index]], StmtRecord::Update { upserted: Some(_) }) => {
                add_count(&mut reply, "nModified", 1)
            }
            None => {}
        }
    }
    sort_by_index(&mut reply, "writeErrors");
    sort_by_index(&mut reply, "upserted");
    let retried: Vec<i32> = retried.iter().map(|i| ids[*i]).collect();
    (with_retried(reply, retried), records)
}

fn retry_find_and_modify(
    handler: Handler,
    doc: &Document,
    ctx: &mut CommandContext,
    ids: &[i32],
    done: &HashMap<i32, StmtRecord>,
) -> Ran {
    let id = ids.first().copied().unwrap_or(0);
    let remove = doc.get("remove").is_some_and(crate::util::truthy);
    if let Some(record) = done.get(&id) {
        let refuse = |code: i32, message: &str| {
            (
                CommandError::new(code, format!("Location{code}"), message).into_reply(),
                Vec::new(),
            )
        };
        let incompatible = |kind: &str| {
            format!(
                "findAndModify retry request is not compatible with previous write in the \
                 transaction of type: {kind}"
            )
        };
        return match record {
            // The reply first given, whatever this request asks to see.
            StmtRecord::FindAndModify {
                reply,
                remove: was_remove,
            } if !was_remove || remove => {
                let mut again = reply.clone();
                let ok = again.remove("ok");
                again.insert("retriedStmtId", id);
                again.insert("ok", ok.unwrap_or(Bson::Double(1.0)));
                (again, Vec::new())
            }
            StmtRecord::FindAndModify { .. } => refuse(40606, &incompatible("d")),
            StmtRecord::Insert { .. } => refuse(40608, &incompatible("i")),
            StmtRecord::Delete if remove => refuse(
                40607,
                "No pre-image available for findAndModify retry request",
            ),
            StmtRecord::Delete => refuse(40606, &incompatible("d")),
            StmtRecord::Update { .. } => refuse(
                40612,
                "findAndModify retry request wants the document before update returned, but \
                 only after update document is stored",
            ),
        };
    }
    let (reply, noted) = transactions::collect_statements(|| crate::run_handler(handler, doc, ctx));
    let records = if ok(&reply) && !noted.is_empty() {
        vec![(
            id,
            StmtRecord::FindAndModify {
                reply: reply.clone(),
                remove,
            },
        )]
    } else {
        Vec::new()
    };
    (reply, records)
}

// ------------------------------------------------------------ session commands

/// Check the list of sessions a session command names, returning their ids.
/// `owner` and `entry` are how mongod names the command and one element of
/// the list in an error.
pub(crate) fn listed_sessions(
    doc: &Document,
    command: &str,
    owner: &str,
    entry: &str,
    strict: bool,
) -> Result<Vec<Vec<u8>>, CommandError> {
    let list = match doc.get(command) {
        Some(Bson::Array(list)) => list,
        Some(other) => return Err(wrong_type(&format!("{owner}.{command}"), other, "array")),
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for item in list {
        match item {
            Bson::Document(d) => out.push(session_id(d, entry)?),
            other => return Err(wrong_type(entry, other, "object")),
        }
    }
    if strict {
        const GENERIC: &[&str] = &[
            "lsid",
            "txnNumber",
            "autocommit",
            "startTransaction",
            "comment",
            "maxTimeMS",
            "readConcern",
            "writeConcern",
            "apiVersion",
            "apiStrict",
            "apiDeprecationErrors",
        ];
        if let Some(key) = doc
            .keys()
            .find(|k| *k != command && !k.starts_with('$') && !GENERIC.contains(&k.as_str()))
        {
            return Err(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("BSON field '{owner}.{key}' is an unknown field."),
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::spec::BinarySubtype;
    use bson::Binary;

    fn uuid() -> Bson {
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: vec![7; 16],
        })
    }

    fn problem(command: Document) -> (i32, String) {
        let name = command.keys().next().unwrap().clone();
        let e = parse(&name, &command).err().expect("refused");
        (e.code, e.errmsg)
    }

    #[test]
    fn a_session_id_must_be_a_uuid() {
        for (lsid, code, message) in [
            (
                Bson::Int32(1),
                14,
                "BSON field 'FindCommandRequest.lsid' is the wrong type 'int', expected type \
                 'object'",
            ),
            (
                Bson::Document(doc! {}),
                40414,
                "BSON field 'FindCommandRequest.lsid.id' is missing but a required field",
            ),
            (
                Bson::Document(doc! {"id": "abc"}),
                14,
                "BSON field 'FindCommandRequest.lsid.id' is the wrong type 'string', expected \
                 type 'binData'",
            ),
            (
                Bson::Document(
                    doc! {"id": Binary { subtype: BinarySubtype::Generic, bytes: vec![0; 16] }},
                ),
                14,
                "BSON field 'FindCommandRequest.lsid.id' is the wrong binData type 'general', \
                 expected type 'UUID'",
            ),
            (
                Bson::Document(
                    doc! {"id": Binary { subtype: BinarySubtype::Uuid, bytes: vec![0; 5] }},
                ),
                207,
                "uuid must be a 16-byte binary field with UUID (4) subtype",
            ),
            (
                Bson::Document(doc! {"id": uuid(), "extra": 1}),
                40415,
                "BSON field 'FindCommandRequest.lsid.extra' is an unknown field.",
            ),
        ] {
            assert_eq!(
                problem(doc! {"find": "c", "lsid": lsid}),
                (code, message.to_string())
            );
        }
    }

    #[test]
    fn the_transaction_fields_are_checked_in_mongods_order() {
        let lsid = doc! {"id": uuid()};
        for (command, code, message) in [
            (
                doc! {"insert": "c", "txnNumber": 1_i64},
                72,
                "Transaction number requires a session ID to also be specified",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "txnNumber": 1_i32},
                14,
                "BSON field 'insert.txnNumber' is the wrong type 'int', expected type 'long'",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "txnNumber": -1_i64},
                72,
                "Transaction number cannot be negative",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "autocommit": false,
                "startTransaction": true},
                72,
                "'autocommit' field requires a transaction number to also be specified",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "txnNumber": 1_i64, "autocommit": true},
                72,
                "Specifying autocommit=true is not allowed.",
            ),
            (
                doc! {"insert": "c", "startTransaction": true},
                72,
                "'startTransaction' field requires 'autocommit' field to also be specified",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "txnNumber": 1_i64,
                "autocommit": false, "startTransaction": false},
                72,
                "Specifying startTransaction=false is not allowed.",
            ),
            (
                doc! {"insert": "c", "lsid": lsid.clone(), "txnNumber": 1_i64, "autocommit": 0},
                14,
                "BSON field 'insert.autocommit' is the wrong type 'int', expected type 'bool'",
            ),
        ] {
            assert_eq!(problem(command), (code, message.to_string()));
        }
        let env = parse(
            "insert",
            &doc! {"insert": "c", "lsid": lsid, "txnNumber": 0_i64, "autocommit": false,
            "startTransaction": true},
        )
        .unwrap_or_else(|e| panic!("refused: {}", e.errmsg));
        assert!(env.in_txn && env.start && env.txn_number == Some(0));
    }

    #[test]
    fn statement_ids_count_up_from_stmt_id() {
        let ids = |d: Document, n| statement_ids("insert", &d, n).map_err(|e| e.code);
        assert_eq!(ids(doc! {}, 3), Ok(vec![0, 1, 2]));
        assert_eq!(ids(doc! {"stmtId": 5}, 2), Ok(vec![5, 6]));
        assert_eq!(ids(doc! {"stmtIds": [7, 6]}, 2), Ok(vec![7, 6]));
        assert_eq!(ids(doc! {"stmtIds": [7, 6]}, 1), Err(16));
        assert_eq!(ids(doc! {"stmtIds": [7], "stmtId": 1}, 1), Err(72));
        assert_eq!(ids(doc! {"stmtId": "x"}, 1), Err(14));
    }

    #[test]
    fn a_stage_a_transaction_refuses_is_named_as_mongod_names_it() {
        let refusal = |stage: &str| {
            stage_refusal(&[Bson::Document(doc! {stage: {}})])
                .map(|e| e.errmsg)
                .unwrap_or_default()
        };
        assert_eq!(refusal("$out"), "$out cannot be used in a transaction");
        assert_eq!(
            refusal("$unionWith"),
            "Stage not supported inside of a multi-document transaction: $unionWith"
        );
        assert_eq!(refusal("$match"), "");
    }
}

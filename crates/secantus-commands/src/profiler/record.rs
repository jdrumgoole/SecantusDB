//! Writing an operation's profile entry, as mongod 8.2.11 writes it.
//!
//! An entry holds only what this server knows about the operation: what it
//! was (`op`, `ns`, `command`), what it did (cursor, counts, error), how long
//! it took, how big its reply was, and who asked. mongod's `locks`,
//! `flowControl`, `storage`, `readConcern` / `writeConcern` provenance, query
//! hashes, `execStats`, and `keysInserted` / `keysDeleted` describe machinery
//! this server does not have, and are left out rather than made up.
//! `keysExamined`, `docsExamined` and `planSummary` are given for a streamed
//! read, which counts them as it reads.
//!
//! Which commands are profiled was measured, one by one: a command mongod
//! never gives a namespace (`ping`, `listCollections`, `validate`, the user
//! and role commands, ...) is not profiled whatever the level, and neither is
//! one refused while its arguments were being read.
//!
//! `update` and `delete` are profiled per STATEMENT: each statement that ran
//! has an entry of its own (`op: "update"` / `"remove"`, the statement as its
//! `command`), and the command itself has none.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use bson::{doc, Bson, Document};

use super::{DbProfile, PROFILE_COLLECTION, PROFILE_COLLECTION_BYTES};
use crate::CommandContext;

thread_local! {
    /// The statements of the `update` / `delete` being profiled on this
    /// thread; `None` while none is.
    static STATEMENTS: RefCell<Option<Vec<Statement>>> = const { RefCell::new(None) };
    /// How many documents the `findAndModify` being profiled changed.
    static MODIFIED: Cell<Option<i64>> = const { Cell::new(None) };
    /// The namespace the handler says the operation is profiled under.
    static NAMESPACE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The operation running on this thread is profiled under `ns` (a command
/// that works through a collection of its own, as `convertToCapped` does).
pub(crate) fn note_namespace(ns: String) {
    NAMESPACE.with(|n| *n.borrow_mut() = Some(ns));
}

/// An operation being profiled, from `dispatch`'s start to its end.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    db: String,
    ns: String,
    /// The database's settings; `None` when only a `$merge` target is
    /// profiled.
    settings: Option<DbProfile>,
    start: Instant,
    /// The handler ran: an operation refused before it did is not profiled.
    reached: bool,
    /// For `create`: the collection was already there, and mongod profiles
    /// nothing.
    existed: bool,
}

/// Start profiling `doc` when its database is profiled -- or has a filter,
/// which decides its slow-query log line too -- or when it is an aggregate
/// whose `$merge` writes to a profiled database.
pub(crate) fn begin(name: &str, doc: &Document, ctx: &CommandContext) -> Option<Pending> {
    // What this operation spills is its own (RM-211).
    secantus_core::memlimit::reset_report();
    let profiler = ctx.profiler.as_ref()?;
    if profiler.held.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return None;
    }
    let (db, ns) = target(name, doc, &ctx.db_name)?;
    let settings = profiler.held(&db);
    // `$merge` writes with update statements, each profiled in the database
    // it writes to (measured).
    let merges = name == "aggregate"
        && merge_target(doc, &ctx.db_name).is_some_and(|(into, _)| profiler.held(&into).is_some());
    if settings.is_none() && !merges {
        return None;
    }
    if matches!(name, "update" | "delete") || merges {
        STATEMENTS.with(|s| *s.borrow_mut() = Some(Vec::new()));
    }
    if matches!(name, "findAndModify" | "findandmodify") {
        MODIFIED.with(|m| m.set(Some(0)));
    }
    let existed = name == "create"
        && ctx.storage.as_ref().is_some_and(|s| {
            let coll = ns.split_once('.').map_or("", |(_, c)| c);
            s.collection_exists(&db, coll).unwrap_or(false)
        });
    Some(Pending {
        db,
        ns,
        settings,
        start: Instant::now(),
        reached: false,
        existed,
    })
}

/// A pipeline's last stage, when it is `name`.
fn last_stage<'a>(doc: &'a Document, name: &str) -> Option<&'a Bson> {
    doc.get_array("pipeline")
        .ok()?
        .last()?
        .as_document()?
        .get(name)
}

/// The database and collection a `$merge` writes to.
fn merge_target(doc: &Document, db: &str) -> Option<(String, String)> {
    let into = match last_stage(doc, "$merge")? {
        Bson::Document(spec) => spec.get("into")?,
        other => other,
    };
    match into {
        Bson::String(coll) => Some((db.to_string(), coll.clone())),
        Bson::Document(d) => Some((
            d.get_str("db").unwrap_or(db).to_string(),
            d.get_str("coll").ok()?.to_string(),
        )),
        _ => None,
    }
}

/// The handler is about to run.
pub(crate) fn mark_reached(ctx: &mut CommandContext) {
    if let Some(p) = ctx.profiling.as_mut() {
        p.reached = true;
    }
}

/// `findAndModify` changed `n` documents.
pub(crate) fn note_modified(n: usize) {
    MODIFIED.with(|m| {
        if m.get().is_some() {
            m.set(Some(n as i64));
        }
    });
}

/// The namespace an operation is profiled under, and so the database whose
/// settings apply: `None` for a command mongod does not profile.
fn target(name: &str, doc: &Document, db: &str) -> Option<(String, String)> {
    let named = |field: &str| match doc.get(field) {
        Some(Bson::String(coll)) if !coll.is_empty() => Some(format!("{db}.{coll}")),
        _ => None,
    };
    let ns = match name {
        "find"
        | "insert"
        | "update"
        | "delete"
        | "count"
        | "distinct"
        | "findAndModify"
        | "findandmodify"
        | "killCursors"
        | "listIndexes"
        | "collStats"
        | "collstats"
        | "create"
        | "createIndexes"
        | "dropIndexes"
        | "deleteIndexes"
        | "drop"
        | "collMod"
        | "collmod"
        | "mapReduce"
        | "mapreduce"
        | "planCacheClear"
        | "planCacheListFilters"
        | "planCacheClearFilters"
        | "convertToCapped" => named(name)?,
        "aggregate" => named(name).unwrap_or_else(|| format!("{db}.$cmd.aggregate")),
        "getMore" => named("collection")?,
        "cloneCollectionAsCapped" => named("toCollection")?,
        "dbStats" | "dbstats" => db.to_string(),
        "explain" => {
            let inner = doc.get_document("explain").ok()?;
            let (_, coll) = inner.iter().next()?;
            match coll {
                Bson::String(c) if !c.is_empty() => format!("{db}.{c}"),
                _ => return None,
            }
        }
        // Run against `admin`, profiled under the collection it renames.
        "renameCollection" => match doc.get(name) {
            Some(Bson::String(source)) if source.contains('.') => source.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let target_db = ns.split('.').next().unwrap_or(db).to_string();
    Some((target_db, ns))
}

/// mongod's `op` for a command.
fn op_of(name: &str, doc: &Document) -> &'static str {
    match name {
        "find" => "query",
        "getMore" => "getmore",
        "insert" => "insert",
        // An `$out` writes, and mongod files the aggregate as an insert.
        "aggregate" if ends_in_out(doc) => "insert",
        _ => "command",
    }
}

fn ends_in_out(doc: &Document) -> bool {
    last_stage(doc, "$out").is_some()
}

/// A refusal mongod gives before it has a namespace, so before profiling:
/// an argument of the wrong type, an unknown or missing field, an unknown
/// pipeline stage, or a `getMore` of a cursor that does not exist. `find`
/// words a wrong-typed field its own way (`Expected field filterto be of
/// type object`, mongod's spelling).
fn refused_unprofiled(name: &str, reply: &Document) -> bool {
    if reply.get_f64("ok").unwrap_or(1.0) == 1.0 {
        // `drop` of a collection that does not exist answers ok, profiled
        // nowhere (measured).
        return name == "drop" && !reply.contains_key("nIndexesWas");
    }
    let code = reply.get_i32("code").unwrap_or(0);
    let msg = reply.get_str("errmsg").unwrap_or("");
    msg.starts_with("BSON field '")
        || msg.starts_with("Expected field ")
        || matches!(code, 40323 | 40324)
        || (name == "getMore" && code == 43)
}

/// Finish profiling the operation `dispatch` ran: write its entry (or its
/// statements' entries) when the settings say so, and record whether a
/// filter wants its slow-query log line. `inner_n` is the reply's `n` before
/// an unacknowledged write zeroed it.
pub(crate) fn finish(
    name: &str,
    doc: &Document,
    inner_n: Option<i64>,
    reply: &Document,
    ctx: &mut CommandContext,
) {
    let statements = STATEMENTS.with(|s| s.borrow_mut().take());
    let modified = MODIFIED.with(|m| m.take());
    let noted_ns = NAMESPACE.with(|n| n.borrow_mut().take());
    let Some(mut pending) = ctx.profiling.take() else {
        return;
    };
    if !pending.reached || pending.existed || refused_unprofiled(name, reply) {
        return;
    }
    if let Some(ns) = noted_ns {
        pending.ns = ns;
    }
    let millis = pending.start.elapsed().as_millis() as i64;
    let who = Who::of(ctx);
    let mut logged = false;
    for statement in statements.unwrap_or_default() {
        let settings = if statement.db == pending.db {
            pending.settings.clone()
        } else {
            ctx.profiler.as_ref().and_then(|p| p.held(&statement.db))
        };
        if let Some(settings) = settings {
            let entry = statement.entry(&who);
            let matched = decide(&statement.db, &settings, &entry, statement.millis, ctx);
            logged |= matched && statement.db == pending.db;
        }
    }
    // `update` and `delete` have their statements' entries and none of
    // their own.
    if let (Some(settings), false) = (&pending.settings, matches!(name, "update" | "delete")) {
        let mut entry = doc! {
            "op": op_of(name, doc),
            "ns": pending.ns.clone(),
            "command": abbreviated(doc),
        };
        let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
        let exec = ctx.exec_stats.as_ref().filter(|_| ok);
        // A cursor's `getMore` names the command that opened it, and repeats
        // its plan (RM-023; measured 8.2.11, 2026-10-24).
        if let Some(command) = exec.and_then(|e| e.originating_command.as_ref()) {
            entry.insert("originatingCommand", abbreviated(command));
        }
        outcome(name, doc, inner_n, modified, reply, ctx, &mut entry);
        entry.insert("responseLength", response_length(reply, ctx));
        entry.insert("protocol", if ctx.op_query { "op_query" } else { "op_msg" });
        entry.insert("millis", int32(millis));
        let summary = exec.and_then(|e| e.plan_summary.clone()).or_else(|| {
            ok.then(|| crate::explain::summary::plan_summary(doc, ctx))
                .flatten()
        });
        if let Some(summary) = summary {
            entry.insert("planSummary", summary);
        }
        who.append(&mut entry);
        logged |= decide(&pending.db, settings, &entry, millis, ctx);
    }
    if pending.settings.is_some_and(|s| s.filter.is_some()) {
        ctx.slow_log = Some(logged);
    }
}

/// Whether `entry` is profiled in `db`, writing it when it is. Returns
/// whether a filter matched it (the slow-query log's decision when one is
/// set).
fn decide(
    db: &str,
    settings: &DbProfile,
    entry: &Document,
    millis: i64,
    ctx: &CommandContext,
) -> bool {
    // A filter that cannot be evaluated against the entry passes nothing,
    // and says why in the log.
    let matched = settings.filter.as_ref().map(|f| {
        f.matches(entry).unwrap_or_else(|e| {
            // mongod writes no line for a filter that throws, at its default
            // verbosity (measured 8.2.11, 2026-10-24).
            log::debug!("The profile filter could not be evaluated: {e}");
            false
        })
    });
    let profiled = match settings.level {
        2 => true,
        1 => matched.unwrap_or_else(|| {
            let worked = std::time::Duration::from_millis(millis.max(0) as u64);
            match &ctx.slow_ops {
                Some(slow) => slow.should_log(worked, rand::random::<f64>()),
                None => millis >= i64::from(crate::slowop::DEFAULT_SLOW_MS),
            }
        }),
        _ => false,
    };
    if profiled {
        write(ctx, db, entry);
    }
    matched.unwrap_or(false)
}

/// Write `entry` to its database's `system.profile`, creating it (capped,
/// 1 MB) when it is missing. A failure is logged, never the client's error:
/// the operation already succeeded or failed on its own.
fn write(ctx: &CommandContext, db: &str, entry: &Document) {
    let Some(storage) = ctx.storage.as_ref() else {
        return;
    };
    let result = (|| -> Result<(), String> {
        if !storage
            .collection_exists(db, PROFILE_COLLECTION)
            .map_err(|e| format!("{e:?}"))?
        {
            let options = doc! { "capped": true, "size": PROFILE_COLLECTION_BYTES };
            storage
                .create_collection_with_options(db, PROFILE_COLLECTION, &options)
                .map_err(|e| format!("{e:?}"))?;
        }
        let mut bytes = Vec::new();
        crate::bsondoc::write(entry, &mut bytes).map_err(|e| format!("{e:?}"))?;
        let (_, errors) = storage
            .insert(db, PROFILE_COLLECTION, vec![bytes], true)
            .map_err(|e| format!("{e:?}"))?;
        match errors.first() {
            Some(e) => Err(e.to_string()),
            None => Ok(()),
        }
    })();
    if let Err(e) = result {
        warn(ctx, entry, &e);
    }
}

/// mongod's warning when an operation's profile entry cannot be written:
/// component `-`, id 20703, with the operation's namespace and the failure
/// (measured 8.2.11, 2026-10-24, through its `forceLockTimeoutForProfiler`
/// fail point).
fn warn(ctx: &CommandContext, entry: &Document, failure: &str) {
    let msg = "Caught Assertion while trying to profile operation";
    let attr = doc! {
        "operation": "msg",
        "namespace": entry.get_str("ns").unwrap_or_default(),
        "assertion": failure,
    };
    match &ctx.logs {
        Some(logs) => logs.log(
            crate::logbuf::LogEntry::new(
                "W",
                "-",
                20703,
                format!("conn{}", ctx.connection_id),
                msg,
            )
            .attr(attr),
        ),
        None => eprintln!("{msg}: {failure}"),
    }
}

/// What the operation did, from its reply, in mongod's field order.
fn outcome(
    name: &str,
    doc: &Document,
    inner_n: Option<i64>,
    modified: Option<i64>,
    reply: &Document,
    ctx: &CommandContext,
    entry: &mut Document,
) {
    let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
    if ok {
        let cursor = reply.get_document("cursor").ok();
        let cursor_id = cursor.and_then(|c| match c.get("id") {
            Some(Bson::Int64(n)) => Some(*n),
            Some(Bson::Int32(n)) => Some(i64::from(*n)),
            _ => None,
        });
        let returned = || -> i64 {
            if let Some(batch) = &ctx.pending_batch {
                return batch.batch.len() as i64;
            }
            cursor
                .and_then(|c| {
                    c.get_array("firstBatch")
                        .or_else(|_| c.get_array("nextBatch"))
                        .ok()
                })
                .map_or(0, |b| b.len() as i64)
        };
        let int = |d: &Document, k: &str| match d.get(k) {
            Some(Bson::Int32(n)) => i64::from(*n),
            Some(Bson::Int64(n)) => *n,
            Some(Bson::Double(n)) => *n as i64,
            _ => 0,
        };
        match name {
            "find" | "getMore" | "aggregate" if cursor.is_some() => {
                // `$out` replaced its target with what it wrote.
                if name == "aggregate" {
                    if let Some(written) = out_count(doc, ctx) {
                        entry.insert("ninserted", int32(written));
                    }
                }
                let id = cursor_id.unwrap_or(0);
                let requested = match doc.get("getMore") {
                    Some(Bson::Int64(n)) => *n,
                    _ => id,
                };
                if name == "getMore" || id != 0 {
                    entry.insert("cursorid", requested);
                }
                // What a streamed read examined for this batch.
                if let Some(e) = &ctx.exec_stats {
                    entry.insert("keysExamined", e.keys_examined);
                    entry.insert("docsExamined", e.docs_examined);
                }
                // A stage that spilled (RM-211): mongod 8.2.11 puts
                // `usedDisk` here and its `$group`'s spill counts after
                // `nreturned`. The counts are this server's own spills.
                let spilled = secantus_core::memlimit::report();
                if spilled.has_sort_stage {
                    entry.insert("hasSortStage", true);
                }
                if spilled.used_disk {
                    entry.insert("usedDisk", true);
                }
                if name != "aggregate" {
                    entry.insert("nBatches", 1_i32);
                }
                if id == 0 {
                    entry.insert("cursorExhausted", true);
                }
                entry.insert("numYield", 0_i32);
                entry.insert("nreturned", int32(returned()));
                if spilled.sort_spills > 0 {
                    entry.insert("sortSpills", spilled.sort_spills);
                    entry.insert("sortSpilledBytes", spilled.sort_spilled_bytes);
                    entry.insert("sortSpilledRecords", spilled.sort_spilled_records);
                    entry.insert(
                        "sortSpilledDataStorageSize",
                        spilled.sort_spilled_data_storage_size,
                    );
                    entry.insert("sortTotalDataSizeBytes", spilled.sort_total_data_size);
                }
                if spilled.group_spills > 0 {
                    entry.insert("groupSpills", spilled.group_spills);
                    entry.insert("groupSpilledBytes", spilled.group_spilled_bytes);
                    entry.insert("groupSpilledRecords", spilled.group_spilled_records);
                    entry.insert(
                        "groupSpilledDataStorageSize",
                        spilled.group_spilled_data_storage_size,
                    );
                }
                return;
            }
            "insert" => {
                entry.insert(
                    "ninserted",
                    int32(inner_n.unwrap_or_else(|| int(reply, "n"))),
                );
            }
            "findAndModify" | "findandmodify" => {
                let last = reply.get_document("lastErrorObject").ok();
                let n = last.map_or(0, |l| int(l, "n"));
                if crate::util::truthy(doc.get("remove").unwrap_or(&Bson::Null)) {
                    entry.insert("ndeleted", int32(n));
                } else {
                    let upserted = last.is_some_and(|l| l.contains_key("upserted"));
                    entry.insert("nMatched", int32(if upserted { 0 } else { n }));
                    entry.insert("nModified", int32(modified.unwrap_or(0)));
                    entry.insert("nUpserted", i32::from(upserted));
                }
                entry.insert("numYield", 0_i32);
                let found = !matches!(reply.get("value"), None | Some(Bson::Null));
                entry.insert("nreturned", i32::from(found));
                return;
            }
            "count" => {
                entry.insert("numYield", 0_i32);
                entry.insert("nreturned", 1_i32);
                return;
            }
            "distinct" => {
                entry.insert("numYield", 0_i32);
                let values = reply.get_array("values").map_or(0, |v| v.len() as i64);
                entry.insert("nreturned", int32(values));
                return;
            }
            _ => {}
        }
        entry.insert("numYield", 0_i32);
        return;
    }
    entry.insert("numYield", 0_i32);
    failure(reply, entry);
}

/// How many documents an `$out` left in its target, which it replaced.
fn out_count(doc: &Document, ctx: &CommandContext) -> Option<i64> {
    let spec = last_stage(doc, "$out")?;
    let (db, coll) = crate::aggregate::out_target(Some(spec), &ctx.db_name).ok()?;
    let storage = ctx.storage.as_ref()?;
    storage
        .count_matching(&db, &coll, &Document::new())
        .ok()
        .map(|n| n as i64)
}

/// The most of a command an entry holds as a document (mongod's 50 KB).
const MAX_COMMAND_BYTES: usize = 50 * 1024;

/// A command as an entry holds it: itself, or past 50 KB mongod's
/// abbreviated rendering of it, `{$truncated: "...", comment}` (strings of
/// 160 characters or more cut to 150, the whole cut to 50 KB).
fn abbreviated(command: &Document) -> Document {
    let size = crate::bsondoc::to_vec(command).map_or(0, |b| b.len());
    if size <= MAX_COMMAND_BYTES {
        return command.clone();
    }
    let mut text = render(&Bson::Document(command.clone()));
    if text.len() > MAX_COMMAND_BYTES {
        let mut cut = MAX_COMMAND_BYTES - 3;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("...");
    }
    let mut out = doc! { "$truncated": text };
    if let Some(comment) = command.get("comment") {
        out.insert("comment", comment.clone());
    }
    out
}

/// mongod's abbreviated rendering of a value (its `toString`).
fn render(value: &Bson) -> String {
    match value {
        Bson::String(s) if s.len() + 1 > 160 => {
            let mut cut = 150;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("\"{}...\"", &s[..cut])
        }
        Bson::Document(d) if !d.is_empty() => format!(
            "{{ {} }}",
            d.iter()
                .map(|(k, v)| format!("{k}: {}", render(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Bson::Array(a) if !a.is_empty() => format!(
            "[ {} ]",
            a.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        other => secantus_core::query::bson_value_repr(other),
    }
}

/// mongod keeps its counts and times as 32-bit integers.
fn int32(n: i64) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// mongod's error fields: `ok: 0`, `errMsg`, `errName`, `errCode`.
fn failure(reply: &Document, entry: &mut Document) {
    entry.insert("ok", 0.0);
    entry.insert("errMsg", reply.get_str("errmsg").unwrap_or(""));
    let code = reply.get_i32("code").unwrap_or(0);
    let name = match reply.get_str("codeName") {
        Ok(name) => name.to_string(),
        Err(_) => crate::mongod_codes::code_name(code)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Location{code}")),
    };
    entry.insert("errName", name);
    entry.insert("errCode", code);
}

/// The size of the reply on the wire: the frame's header and sections around
/// the reply document, the cursor batch included.
fn response_length(reply: &Document, ctx: &CommandContext) -> i32 {
    let body = crate::bsondoc::to_vec(reply).map_or(0, |b| b.len());
    let batch = ctx.pending_batch.as_ref().map_or(0, |pending| {
        // The batch is an array element of the reply's `cursor`.
        let items: usize = pending
            .batch
            .iter()
            .enumerate()
            .map(|(i, blob)| 2 + i.to_string().len() + blob.len())
            .sum();
        2 + pending.batch_field.len() + 4 + items + 1
    });
    // OP_MSG: 16-byte header, 4 flag bytes, a section-kind byte. OP_REPLY:
    // 16-byte header, then flags, cursor id, starting point and count.
    let frame = if ctx.op_query { 36 } else { 21 };
    (frame + body + batch) as i32
}

/// Who ran the operation, as the entry's last fields.
struct Who {
    client: String,
    app_name: Option<String>,
    users: Vec<(String, String)>,
}

impl Who {
    fn of(ctx: &CommandContext) -> Self {
        let auth = ctx.conn_auth.as_ref().and_then(|a| a.lock().ok());
        let peer = auth
            .as_ref()
            .and_then(|a| a.peer_address.clone())
            .unwrap_or_default();
        // mongod gives the client's address without its port.
        let client = match peer.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
            _ => peer,
        };
        let app_name = auth.as_ref().and_then(|a| {
            a.client_metadata
                .as_ref()?
                .get_document("application")
                .ok()?
                .get_str("name")
                .ok()
                .map(str::to_string)
        });
        let users = auth.map(|a| a.authenticated.clone()).unwrap_or_default();
        Who {
            client,
            app_name,
            users,
        }
    }

    fn append(&self, entry: &mut Document) {
        entry.insert("ts", bson::DateTime::now());
        entry.insert("client", self.client.clone());
        if let Some(app) = &self.app_name {
            entry.insert("appName", app.clone());
        }
        let all: Vec<Bson> = self
            .users
            .iter()
            .map(|(db, user)| Bson::Document(doc! {"user": user.clone(), "db": db.clone()}))
            .collect();
        entry.insert("allUsers", all);
        let user = self
            .users
            .first()
            .map_or(String::new(), |(db, user)| format!("{user}@{db}"));
        entry.insert("user", user);
    }
}

/// One statement of a profiled `update` or `delete` that ran.
struct Statement {
    db: String,
    ns: String,
    op: &'static str,
    command: Document,
    outcome: StatementOutcome,
    millis: i64,
}

enum StatementOutcome {
    Updated {
        matched: i64,
        modified: i64,
        upserted: i64,
    },
    Deleted(i64),
    Failed(Document),
}

impl Statement {
    /// The statement's `Slow query` line, before its duration: mongod's
    /// attributes for a write statement, in its order (measured 8.2.11,
    /// 2026-10-24), as far as this server keeps them (RM-023).
    fn log_attributes(&self) -> Document {
        let mut attr =
            doc! { "type": self.op, "ns": self.ns.clone(), "command": self.command.clone() };
        match &self.outcome {
            StatementOutcome::Updated {
                matched,
                modified,
                upserted,
            } => {
                attr.insert("nMatched", *matched);
                attr.insert("nModified", *modified);
                attr.insert("nUpserted", *upserted);
            }
            StatementOutcome::Deleted(n) => {
                attr.insert("ndeleted", *n);
            }
            StatementOutcome::Failed(error) => {
                attr.insert("ok", 0_i32);
                attr.insert("errCode", error.get_i32("code").unwrap_or(0));
            }
        }
        attr.insert("numYields", 0_i32);
        attr
    }

    fn entry(&self, who: &Who) -> Document {
        let mut entry =
            doc! { "op": self.op, "ns": self.ns.clone(), "command": abbreviated(&self.command) };
        match &self.outcome {
            StatementOutcome::Updated {
                matched,
                modified,
                upserted,
            } => {
                entry.insert("nMatched", int32(*matched));
                entry.insert("nModified", int32(*modified));
                entry.insert("nUpserted", int32(*upserted));
                entry.insert("numYield", 0_i32);
            }
            StatementOutcome::Deleted(n) => {
                entry.insert("ndeleted", int32(*n));
                entry.insert("numYield", 0_i32);
            }
            StatementOutcome::Failed(error) => {
                entry.insert("numYield", 0_i32);
                let reply = doc! {
                    "errmsg": error.get_str("errmsg").unwrap_or(""),
                    "code": error.get_i32("code").unwrap_or(0),
                };
                failure(&reply, &mut entry);
            }
        }
        entry.insert("millis", int32(self.millis));
        who.append(&mut entry);
        entry
    }
}

/// The running tally of a write command's statements, kept by the `update`
/// and `delete` handlers; it does nothing unless the command is profiled.
/// A statement is opened when it starts to execute (one refused before that
/// is not profiled, as on mongod) and closed by the next one or the end.
pub(crate) struct Statements {
    armed: bool,
    /// The command is profiled, so each statement gets an entry.
    profiled: bool,
    open: Option<Open>,
}

struct Open {
    db: String,
    coll: String,
    op: &'static str,
    command: Document,
    start: Instant,
    n: i64,
    modified: i64,
    upserted: usize,
    errors: usize,
}

impl Statements {
    pub(crate) fn start() -> Self {
        let profiled = STATEMENTS.with(|s| s.borrow().is_some());
        Statements {
            // A statement is timed when it is profiled or may be logged as
            // slow (`slowop::statement`).
            armed: profiled || crate::slowop::armed(),
            profiled,
            open: None,
        }
    }

    /// The update statement `spec` of `db.coll` starts to execute, with the
    /// command's tallies as they stand.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        db: &str,
        coll: &str,
        spec: &Document,
        n: i32,
        modified: i32,
        upserted: usize,
        errors: usize,
    ) {
        if self.armed {
            self.open = Some(Open {
                db: db.to_string(),
                coll: coll.to_string(),
                op: "update",
                command: update_statement(spec),
                start: Instant::now(),
                n: i64::from(n),
                modified: i64::from(modified),
                upserted,
                errors,
            });
        }
    }

    /// The delete statement `spec` of `db.coll` starts to execute.
    pub(crate) fn delete(&mut self, db: &str, coll: &str, spec: &Document, n: i32, errors: usize) {
        if self.armed {
            self.open = Some(Open {
                db: db.to_string(),
                coll: coll.to_string(),
                op: "remove",
                command: delete_statement(spec),
                start: Instant::now(),
                n: i64::from(n),
                modified: 0,
                upserted: 0,
                errors,
            });
        }
    }

    /// Close the open statement, if any, against the tallies now.
    pub(crate) fn close(&mut self, n: i32, modified: i32, upserted: usize, errors: &[Bson]) {
        let Some(open) = self.open.take() else {
            return;
        };
        let outcome = if errors.len() > open.errors {
            StatementOutcome::Failed(
                errors
                    .last()
                    .and_then(Bson::as_document)
                    .cloned()
                    .unwrap_or_default(),
            )
        } else if open.op == "remove" {
            StatementOutcome::Deleted(i64::from(n) - open.n)
        } else {
            let upserted = (upserted - open.upserted) as i64;
            StatementOutcome::Updated {
                matched: i64::from(n) - open.n - upserted,
                modified: i64::from(modified) - open.modified,
                upserted,
            }
        };
        let worked = open.start.elapsed();
        let statement = Statement {
            ns: format!("{}.{}", open.db, open.coll),
            db: open.db,
            op: open.op,
            command: open.command,
            outcome,
            millis: worked.as_millis() as i64,
        };
        crate::slowop::statement("WRITE", statement.log_attributes(), worked);
        if self.profiled {
            STATEMENTS.with(|s| {
                if let Some(list) = s.borrow_mut().as_mut() {
                    list.push(statement);
                }
            });
        }
    }
}

/// An update statement as mongod records it: its fields in mongod's order,
/// with `multi` and `upsert` always present.
fn update_statement(spec: &Document) -> Document {
    let mut out = Document::new();
    for key in ["q", "u", "c", "arrayFilters"] {
        if let Some(v) = spec.get(key) {
            out.insert(key, v.clone());
        }
    }
    hint(spec, &mut out);
    let flag = |k: &str| crate::util::truthy(spec.get(k).unwrap_or(&Bson::Boolean(false)));
    out.insert("multi", flag("multi"));
    out.insert("upsert", flag("upsert"));
    for key in ["upsertSupplied", "collation", "sort"] {
        if let Some(v) = spec.get(key) {
            out.insert(key, v.clone());
        }
    }
    out
}

/// A delete statement as mongod records it.
fn delete_statement(spec: &Document) -> Document {
    let mut out = Document::new();
    for key in ["q", "limit"] {
        if let Some(v) = spec.get(key) {
            out.insert(key, v.clone());
        }
    }
    hint(spec, &mut out);
    if let Some(v) = spec.get("collation") {
        out.insert("collation", v.clone());
    }
    out
}

/// A statement's `hint`, when it names one.
fn hint(spec: &Document, out: &mut Document) {
    match spec.get("hint") {
        Some(Bson::Document(d)) if d.is_empty() => {}
        Some(Bson::String(s)) if s.is_empty() => {}
        Some(v) => {
            out.insert("hint", v.clone());
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_namespace_is_the_operations() {
        assert_eq!(
            target("find", &doc! {"find": "t"}, "a"),
            Some(("a".into(), "a.t".into()))
        );
        assert_eq!(
            target("aggregate", &doc! {"aggregate": 1}, "a"),
            Some(("a".into(), "a.$cmd.aggregate".into()))
        );
        assert_eq!(
            target(
                "renameCollection",
                &doc! {"renameCollection": "b.x", "to": "b.y"},
                "admin"
            ),
            Some(("b".into(), "b.x".into()))
        );
        assert_eq!(
            target("dbStats", &doc! {"dbStats": 1}, "a"),
            Some(("a".into(), "a".into()))
        );
        assert_eq!(
            target("listCollections", &doc! {"listCollections": 1}, "a"),
            None
        );
        assert_eq!(target("ping", &doc! {"ping": 1}, "a"), None);
    }

    /// Rendered as mongod 8.2.11 rendered it (2026-10-15).
    #[test]
    fn a_command_past_fifty_kilobytes_is_abbreviated() {
        let small = doc! {"find": "t", "filter": {"pad": "x".repeat(200)}};
        assert_eq!(abbreviated(&small), small);
        let big = doc! {
            "find": "t",
            "filter": {"a": "y".repeat(60_000), "n": 5, "arr": [1, 2, {"q": "w"}]},
            "batchSize": 3,
            "comment": "c",
        };
        let want = format!(
            "{{ find: \"t\", filter: {{ a: \"{}...\", n: 5, arr: [ 1, 2, {{ q: \"w\" }} ] }}, \
             batchSize: 3, comment: \"c\" }}",
            "y".repeat(150)
        );
        assert_eq!(abbreviated(&big), doc! {"$truncated": want, "comment": "c"});
        let long_key = doc! {"find": "t", "projection": {"x".repeat(60_000): 1}};
        let text = abbreviated(&long_key)
            .get_str("$truncated")
            .unwrap()
            .to_string();
        assert_eq!((text.len(), text.ends_with("xx...")), (51_200, true));
    }

    #[test]
    fn statements_read_as_mongod_records_them() {
        assert_eq!(
            update_statement(
                &doc! {"upsert": true, "u": {"$set": {"a": 1}}, "q": {"_id": 1},
                "hint": {"_id": 1}, "collation": {"locale": "simple"}}
            ),
            doc! {"q": {"_id": 1}, "u": {"$set": {"a": 1}}, "hint": {"_id": 1},
            "multi": false, "upsert": true, "collation": {"locale": "simple"}}
        );
        assert_eq!(
            delete_statement(&doc! {"limit": 0, "q": {"a": 2}}),
            doc! {"q": {"a": 2}, "limit": 0}
        );
    }
}

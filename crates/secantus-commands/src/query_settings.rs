//! Query settings: `setQuerySettings`, `removeQuerySettings`, the
//! `$querySettings` stage, and the settings a `find`, `distinct` or
//! `aggregate` runs under (RM-219).
//!
//! mongod 8.2 keys a setting by the `queryShapeHash` of a query
//! (`query_shape.rs`) and keeps them all in one cluster parameter document,
//! `config.clusterParameters` `{_id: "querySettings", clusterParameterTime,
//! settingsArray: [{queryShapeHash, settings, representativeQuery}]}`, which
//! `getClusterParameter` does not list. A setting is
//! `{indexHints, queryFramework, reject, comment}`, always in that order; a
//! second `setQuerySettings` for one shape merges into the first, a new
//! `indexHints` replacing the old. Every rule and message below was measured
//! on 8.2.1 and 8.2.11 (one-member replica set and standalone, 2026-11-08).
//!
//! A read whose shape has a setting:
//! - is refused with 411 `QueryRejectedBySettings` under `reject: true` (its
//!   `explain` is not);
//! - plans over the `allowedIndexes` named for its collection, by name or key
//!   pattern, and scans the collection only when `{$natural: 1 | -1}` is
//!   among them; when none of them can serve the query and no `$natural` is
//!   allowed, it plans as if there were no hints. Its own `hint` and any index
//!   filter are ignored, and its `explain` says `querySettings` and
//!   `indexFilterSet: false`.
//!
//! Not yet: `queryFramework` is kept and reported but does not choose the
//! engine, and a write made to `config.clusterParameters` directly is read
//! only by the next server start (RM-418).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use bson::{doc, Bson, Document};

use crate::{CommandContext, CommandError, HandlerResult};

const PARAMS_DB: &str = "config";
const PARAMS_COLL: &str = "clusterParameters";
const PARAM: &str = "querySettings";

/// The server's settings, as last read from or written to
/// `config.clusterParameters`: `None` until first read.
#[derive(Default)]
pub struct QuerySettingsStore {
    entries: Mutex<Option<Vec<Document>>>,
}

impl QuerySettingsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn error(code: i32, msg: impl Into<String>) -> CommandError {
    CommandError::new(code, crate::util::error_code_name(code), msg)
}

fn type_name(v: &Bson) -> &'static str {
    secantus_core::query::bson_type_name(v)
}

fn repr(d: &Document) -> String {
    secantus_core::query::bson_value_repr(&Bson::Document(d.clone()))
}

// ------------------------------------------------------------- storage --

/// The stored parameter document, if one was ever written.
fn stored(storage: &dyn crate::storage::Storage) -> Result<Option<Document>, CommandError> {
    let rows = storage
        .find(PARAMS_DB, PARAMS_COLL, &doc! {"_id": PARAM}, None, None)
        .map_err(crate::util::command_error)?;
    Ok(rows
        .first()
        .and_then(|raw| crate::bsondoc::from_bytes(raw).ok()))
}

fn entries_of(row: Option<&Document>) -> Vec<Document> {
    row.and_then(|r| r.get_array("settingsArray").ok())
        .map(|a| a.iter().filter_map(|e| e.as_document().cloned()).collect())
        .unwrap_or_default()
}

/// Every setting, read from the store once and kept in memory after.
fn entries(ctx: &mut CommandContext) -> Result<Vec<Document>, CommandError> {
    let store = ctx
        .query_settings
        .get_or_insert_with(|| std::sync::Arc::new(QuerySettingsStore::new()))
        .clone();
    let mut guard = store.entries.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entries) = guard.as_ref() {
        return Ok(entries.clone());
    }
    let loaded = entries_of(stored(ctx.storage()?)?.as_ref());
    *guard = Some(loaded.clone());
    Ok(loaded)
}

/// Write `entries` as the parameter document and keep them in memory.
fn save(ctx: &mut CommandContext, entries: Vec<Document>) -> Result<(), CommandError> {
    let storage = ctx.storage()?;
    let row = doc! {
        "_id": PARAM,
        "clusterParameterTime": storage.current_cluster_time(),
        "settingsArray": entries.iter().cloned().map(Bson::Document).collect::<Vec<_>>(),
    };
    storage
        .update_matching(
            PARAMS_DB,
            PARAMS_COLL,
            &doc! {"_id": PARAM},
            &row,
            false,
            true,
        )
        .map_err(crate::util::command_error)?;
    let store = ctx
        .query_settings
        .get_or_insert_with(|| std::sync::Arc::new(QuerySettingsStore::new()))
        .clone();
    *store.entries.lock().unwrap_or_else(|e| e.into_inner()) = Some(entries);
    Ok(())
}

/// `serverStatus.querySettings`: how many settings there are, how many
/// reject, and the size of the parameter document that keeps them (0 before
/// the first is set).
pub(crate) fn server_status_section(ctx: &CommandContext) -> Document {
    let Ok(storage) = ctx.storage() else {
        return doc! {"count": 0_i32, "size": 0_i32, "rejectCount": 0_i32};
    };
    let row = stored(storage).ok().flatten();
    let entries = entries_of(row.as_ref());
    let size = row.as_ref().map_or(0, |r| {
        let kept = doc! {
            "_id": PARAM,
            "clusterParameterTime": r.get("clusterParameterTime").cloned().unwrap_or(Bson::Null),
            "settingsArray": entries.iter().cloned().map(Bson::Document).collect::<Vec<_>>(),
        };
        crate::bsondoc::to_vec(&kept).map_or(0, |b| b.len())
    });
    let rejecting = entries
        .iter()
        .filter(|e| {
            e.get_document("settings")
                .is_ok_and(|s| s.get_bool("reject") == Ok(true))
        })
        .count();
    doc! {
        "count": entries.len() as i32,
        "size": size as i32,
        "rejectCount": rejecting as i32,
    }
}

// -------------------------------------------------------------- parsing --

/// The settings a command sends, parsed as mongod's IDL parses them: types
/// checked, unknown fields dropped, the fields in mongod's order.
fn parse_settings(v: Option<&Bson>) -> Result<Document, CommandError> {
    let settings = match v {
        None => {
            return Err(error(
                40414,
                "BSON field 'setQuerySettings.settings' is missing but a required field",
            ))
        }
        Some(Bson::Document(d)) => d,
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'setQuerySettings.settings' is the wrong type '{}', expected \
                     type 'object'",
                    type_name(other)
                ),
            ))
        }
    };
    let mut out = Document::new();
    if let Some(hints) = settings.get("indexHints") {
        let specs: Vec<&Bson> = match hints {
            Bson::Document(_) => vec![hints],
            Bson::Array(items) => items.iter().collect(),
            _ => return Err(error(9, "'indexHints' must be an object or an array")),
        };
        let mut parsed = Vec::new();
        for spec in specs {
            if let Some(hint) = parse_index_hint(spec)? {
                parsed.push(Bson::Document(hint));
            }
        }
        out.insert("indexHints", parsed);
    }
    match settings.get("queryFramework") {
        None => {}
        Some(Bson::String(s)) if s == "classic" || s == "sbe" => {
            out.insert("queryFramework", s.clone());
        }
        Some(Bson::String(s)) => {
            return Err(error(
                2,
                format!("Invalid value for 'queryFramework': expected classic or sbe, but got {s}"),
            ))
        }
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'setQuerySettings.settings.queryFramework' is the wrong type \
                     '{}', expected type 'string'",
                    type_name(other)
                ),
            ))
        }
    }
    match settings.get("reject") {
        None => {}
        Some(Bson::Boolean(b)) => {
            out.insert("reject", *b);
        }
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "Field 'reject' should be a boolean value, but found: {}",
                    type_name(other)
                ),
            ))
        }
    }
    if let Some(comment) = settings.get("comment") {
        out.insert("comment", comment.clone());
    }
    Ok(out)
}

/// One `IndexHintSpec`, or `None` for one that allows no index (it is
/// dropped).
fn parse_index_hint(spec: &Bson) -> Result<Option<Document>, CommandError> {
    let Bson::Document(spec) = spec else {
        return Err(error(9, "'indexHints' must be an object or an array"));
    };
    let ns = match spec.get("ns") {
        None => {
            return Err(error(
                40414,
                "BSON field 'IndexHintSpec.ns' is missing but a required field",
            ))
        }
        Some(Bson::Document(d)) => d,
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'IndexHintSpec.ns' is the wrong type '{}', expected type 'object'",
                    type_name(other)
                ),
            ))
        }
    };
    let mut namespace = Document::new();
    for (k, v) in ns {
        if k != "db" && k != "coll" {
            return Err(error(
                40415,
                format!("BSON field 'IndexHintSpec.ns.{k}' is an unknown field."),
            ));
        }
        let Bson::String(s) = v else {
            return Err(error(
                14,
                format!(
                    "BSON field 'IndexHintSpec.ns.{k}' is the wrong type '{}', expected type \
                     'string'",
                    type_name(v)
                ),
            ));
        };
        namespace.insert(k, s.clone());
    }
    let allowed = match spec.get("allowedIndexes") {
        None => {
            return Err(error(
                40414,
                "BSON field 'IndexHintSpec.allowedIndexes' is missing but a required field",
            ))
        }
        Some(Bson::Array(a)) => a,
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field 'IndexHintSpec.allowedIndexes' is the wrong type '{}', expected \
                     type 'array'",
                    type_name(other)
                ),
            ))
        }
    };
    for a in allowed {
        if !matches!(a, Bson::String(_) | Bson::Document(_)) {
            return Err(error(9, "Hint must be a string or an object"));
        }
    }
    if allowed.is_empty() {
        return Ok(None);
    }
    // mongod writes `db` before `coll` whatever order they came in.
    let mut ordered = Document::new();
    for k in ["db", "coll"] {
        if let Some(v) = namespace.get(k) {
            ordered.insert(k, v.clone());
        }
    }
    Ok(Some(
        doc! {"ns": ordered, "allowedIndexes": allowed.clone()},
    ))
}

/// The checks mongod makes of the index hints once the query is known.
fn check_index_hints(settings: &Document) -> Result<(), CommandError> {
    let Ok(hints) = settings.get_array("indexHints") else {
        return Ok(());
    };
    let mut seen: Vec<&Document> = Vec::new();
    for hint in hints.iter().filter_map(Bson::as_document) {
        let ns = hint.get_document("ns").ok();
        if !ns.is_some_and(|n| n.contains_key("db")) {
            return Err(error(
                8727500,
                "invalid index hint: 'ns.db' field is missing",
            ));
        }
        if !ns.is_some_and(|n| n.contains_key("coll")) {
            return Err(error(
                8727501,
                "invalid index hint: 'ns.coll' field is missing",
            ));
        }
        if let Some(first) = seen.iter().find(|s| s.get_document("ns").ok() == ns) {
            return Err(error(
                7746608,
                format!(
                    "Collection '{}' has already index hints specified",
                    repr(first)
                ),
            ));
        }
        seen.push(hint);
    }
    Ok(())
}

/// A hash given as text: 64 hex digits, either case, answered uppercase.
fn parse_hash(text: &str) -> Result<String, CommandError> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error(2, "Hash input is not a hex string"));
    }
    if text.len() != 64 {
        return Err(error(
            16,
            format!("Unsupported SHA256Block hash length: {}", text.len() / 2),
        ));
    }
    Ok(text.to_ascii_uppercase())
}

/// The fields each representative query may carry beside the generic ones.
fn known_fields(command: &str) -> &'static [&'static str] {
    match command {
        "find" => crate::find::FIND_FIELDS,
        "distinct" => crate::distinct::DISTINCT_FIELDS,
        _ => crate::aggregate::AGGREGATE_FIELDS,
    }
}

/// A query given as an instance: its database, and its hash when this
/// server can shape it. Parsed as mongod parses the command it names.
fn parse_query(query: &Document) -> Result<(String, String), CommandError> {
    let name = query.keys().next().map(String::as_str).unwrap_or("");
    if !matches!(name, "find" | "distinct" | "aggregate") {
        return Err(error(
            7746402,
            format!(
                "QueryShape can not be computed for command: {}",
                repr(query)
            ),
        ));
    }
    let idl = match name {
        "find" => "findCommandRequest",
        "distinct" => "distinctCommandRequest",
        _ => "aggregateCommandRequest",
    };
    for key in query.keys().skip(1) {
        if !known_fields(name).contains(&key.as_str())
            && !crate::params::is_generic_arg(key)
            && key != "$db"
        {
            return Err(error(
                40415,
                format!("BSON field '{idl}.{key}' is an unknown field."),
            ));
        }
    }
    let db = match query.get("$db") {
        None => {
            return Err(error(
                40414,
                format!("BSON field '{idl}.$db' is missing but a required field"),
            ))
        }
        Some(Bson::String(s)) => s.clone(),
        Some(other) => {
            return Err(error(
                14,
                format!(
                    "BSON field '{idl}.$db' is the wrong type '{}', expected type 'string'",
                    type_name(other)
                ),
            ))
        }
    };
    let coll = match query.get(name) {
        Some(Bson::String(s)) => s.clone(),
        Some(Bson::Int32(1) | Bson::Int64(1)) if name == "aggregate" => String::new(),
        Some(Bson::Double(d)) if name == "aggregate" && *d == 1.0 => String::new(),
        Some(other) => {
            return Err(error(
                73,
                format!("Collection name has invalid type {}", type_name(other)),
            ))
        }
        None => unreachable!("the command's own field is its first"),
    };
    let filter_key = if name == "distinct" {
        "query"
    } else {
        "filter"
    };
    if let Ok(filter) = query.get_document(filter_key) {
        if let Some(secantus_core::fallback::Fallback::Mongo { code, message, .. }) =
            secantus_core::query::unknown_top_level_refusal(filter)
        {
            return Err(error(code, message));
        }
    }
    Ok((db, coll))
}

/// Whether a setting can exist for a query of `db.coll`.
fn settable(db: &str, coll: &str) -> bool {
    !matches!(db, "admin" | "local" | "config") && !coll.starts_with("system.")
}

/// The query's `queryShapeHash`, or a refusal by name where this server
/// does not shape it (`query_shape.rs`).
fn hash_of(query: &Document, db: &str, cmd: &str) -> Result<String, CommandError> {
    crate::query_shape::hash(db, query).ok_or_else(|| {
        error(
            115,
            format!(
                "{cmd} is not supported by SecantusDB for this query: its query shape \
                 is not computed here"
            ),
        )
    })
}

/// mongod's `isIdHackEligibleQuery` for a representative `find`: a filter
/// that is `{_id: <value>}` alone, with no `skip`, `hint`, `min`, `max`,
/// `showRecordId` or `tailable`.
fn id_hack_eligible(query: &Document) -> bool {
    if query.keys().next().map(String::as_str) != Some("find") {
        return false;
    }
    if ["skip", "hint", "min", "max"]
        .iter()
        .any(|k| query.get(k).is_some_and(|v| !matches!(v, Bson::Null)))
        || query.get_bool("showRecordId") == Ok(true)
        || query.get_bool("tailable") == Ok(true)
    {
        return false;
    }
    let Ok(filter) = query.get_document("filter") else {
        return false;
    };
    if filter.len() != 1 {
        return false;
    }
    match filter.get("_id") {
        Some(Bson::Array(_) | Bson::RegularExpression(_)) | None => false,
        Some(Bson::Document(d)) => match d.iter().next() {
            Some((k, v)) if k == "$eq" => {
                d.len() == 1 && !matches!(v, Bson::Array(_) | Bson::RegularExpression(_))
            }
            Some((k, _)) => !k.starts_with('$'),
            None => true,
        },
        Some(_) => true,
    }
}

/// Whether `settings` holds anything but defaults (`comment` aside).
fn only_defaults(settings: &Document) -> bool {
    !settings.contains_key("indexHints")
        && !settings.contains_key("queryFramework")
        && !settings.contains_key("reject")
}

/// `existing` with `new` merged over it, in mongod's field order; a
/// `reject: false` unsets it.
fn merged(existing: Option<&Document>, new: &Document) -> Document {
    let mut out = Document::new();
    for key in ["indexHints", "queryFramework", "reject", "comment"] {
        let value = new.get(key).or_else(|| existing.and_then(|e| e.get(key)));
        match (key, value) {
            ("indexHints", Some(Bson::Array(a))) if a.is_empty() => {}
            ("reject", Some(Bson::Boolean(false))) => {}
            (_, Some(v)) => {
                out.insert(key, v.clone());
            }
            (_, None) => {}
        }
    }
    out
}

fn unknown_field(doc: &Document, cmd: &str, allowed: &[&str]) -> Result<(), CommandError> {
    for key in doc.keys() {
        if !allowed.contains(&key.as_str()) && !crate::params::is_generic_arg(key) {
            return Err(error(
                40415,
                format!("BSON field '{cmd}.{key}' is an unknown field."),
            ));
        }
    }
    Ok(())
}

/// The command's argument: a hash, or a query instance.
enum Target {
    Hash(String),
    Query(Document),
}

fn target(doc: &Document, cmd: &str) -> Result<Target, CommandError> {
    match doc.get(cmd) {
        Some(Bson::String(s)) => Ok(Target::Hash(parse_hash(s)?)),
        Some(Bson::Document(d)) => Ok(Target::Query(d.clone())),
        Some(other) => Err(error(
            14,
            format!(
                "BSON field '{cmd}.{cmd}' is the wrong type '{}', expected types '[string, object]'",
                type_name(other)
            ),
        )),
        None => unreachable!("the command's own field is its first"),
    }
}

fn require_replica_set(ctx: &CommandContext, cmd: &str) -> Result<(), CommandError> {
    if ctx.replica_set_name.is_none() {
        return Err(error(
            20,
            format!("{cmd} can only run on replica sets or sharded clusters"),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------- commands --

/// `setQuerySettings`.
pub fn set_query_settings(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    const CMD: &str = "setQuerySettings";
    unknown_field(doc, CMD, &[CMD, "settings"])?;
    let target = target(doc, CMD)?;
    let settings = parse_settings(doc.get("settings"))?;
    if settings.is_empty() {
        return Err(error(
            8727502,
            "settings field in setQuerySettings command cannot be empty",
        ));
    }
    require_replica_set(ctx, CMD)?;
    let (hash, query) = match target {
        Target::Hash(hash) => (hash, None),
        Target::Query(query) => {
            let (db, coll) = parse_query(&query)?;
            if matches!(db.as_str(), "admin" | "local" | "config") {
                return Err(error(
                    8584900,
                    "setQuerySettings command cannot be used on internal databases",
                ));
            }
            if coll.starts_with("system.") {
                return Err(error(
                    8584901,
                    "setQuerySettings command cannot be used on system collections",
                ));
            }
            if id_hack_eligible(&query) {
                return Err(error(
                    7746606,
                    "setQuerySettings command cannot be used on find queries eligible for IDHACK",
                ));
            }
            (hash_of(&query, &db, CMD)?, Some(query))
        }
    };
    check_index_hints(&settings)?;
    let mut all = entries(ctx)?;
    let position = all
        .iter()
        .position(|e| e.get_str("queryShapeHash") == Ok(hash.as_str()));
    let existing = position.map(|i| all[i].clone());
    let result = merged(
        existing
            .as_ref()
            .and_then(|e| e.get_document("settings").ok()),
        &settings,
    );
    if only_defaults(&result) {
        return Err(error(
            7746604,
            "the resulting settings cannot be empty or contain only default values",
        ));
    }
    let representative = query.or_else(|| {
        existing
            .as_ref()
            .and_then(|e| e.get_document("representativeQuery").ok().cloned())
    });
    let mut entry = doc! {"queryShapeHash": hash, "settings": result};
    if let Some(q) = representative {
        entry.insert("representativeQuery", q);
    }
    match position {
        Some(i) => all[i] = entry.clone(),
        None => all.push(entry.clone()),
    }
    save(ctx, all)?;
    entry.insert("ok", 1.0);
    Ok(entry)
}

/// `removeQuerySettings`: no error for a shape that has none.
pub fn remove_query_settings(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    const CMD: &str = "removeQuerySettings";
    unknown_field(doc, CMD, &[CMD])?;
    let hash = match target(doc, CMD)? {
        Target::Hash(hash) => hash,
        Target::Query(query) => {
            let (db, coll) = parse_query(&query)?;
            if !settable(&db, &coll) {
                // No setting can be kept for it: nothing to remove.
                require_replica_set(ctx, CMD)?;
                return Ok(doc! {"ok": 1.0});
            }
            hash_of(&query, &db, CMD)?
        }
    };
    require_replica_set(ctx, CMD)?;
    let mut all = entries(ctx)?;
    let before = all.len();
    all.retain(|e| e.get_str("queryShapeHash") != Ok(hash.as_str()));
    if all.len() != before {
        save(ctx, all)?;
    }
    Ok(doc! {"ok": 1.0})
}

/// `$querySettings`'s argument: `{showDebugQueryShape: <bool>}`.
pub(crate) fn validate_stage(spec: Option<&Bson>) -> Result<bool, CommandError> {
    let Some(Bson::Document(spec)) = spec else {
        return Err(error(
            7746800,
            "$querySettings stage expects a document as argument",
        ));
    };
    let mut show = false;
    for (k, v) in spec {
        if k != "showDebugQueryShape" {
            return Err(error(
                40415,
                format!("BSON field '$querySettings.{k}' is an unknown field."),
            ));
        }
        show = match v {
            Bson::Boolean(b) => *b,
            other => {
                return Err(error(
                    14,
                    format!(
                        "BSON field '$querySettings.showDebugQueryShape' is the wrong type \
                         '{}', expected type 'bool'",
                        type_name(other)
                    ),
                ))
            }
        };
    }
    Ok(show)
}

/// `$querySettings`'s documents: every setting. `showDebugQueryShape` is
/// parsed but its `debugQueryShape` is not given (RM-418).
pub(crate) fn stage_rows(
    storage: &dyn crate::storage::Storage,
    spec: Option<&Bson>,
) -> Result<Vec<Document>, CommandError> {
    validate_stage(spec)?;
    Ok(entries_of(stored(storage)?.as_ref()))
}

// ------------------------------------------- the settings a read runs under --

thread_local! {
    /// The settings of the read this thread is running, for the planner.
    static ACTIVE: RefCell<Option<Rc<Document>>> = const { RefCell::new(None) };
}

/// Holds a read's settings in place until the read ends.
pub(crate) struct Scope {
    previous: Option<Rc<Document>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        ACTIVE.with(|a| *a.borrow_mut() = previous);
    }
}

/// The read `doc` names, and whether it is explained.
fn read_of(doc: &Document) -> Option<(&Document, bool)> {
    match doc.keys().next().map(String::as_str)? {
        "find" | "distinct" | "aggregate" => Some((doc, false)),
        "explain" => {
            let inner = doc.get_document("explain").ok()?;
            match inner.keys().next().map(String::as_str)? {
                "find" | "distinct" | "aggregate" => Some((inner, true)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Before a command runs: the settings its read's shape has, put in place
/// for its planning, or its refusal under `reject: true`. `None` for a
/// command that is not such a read, or one whose shape has no setting.
/// Also the request as the read is planned: without its own `hint` when
/// index hints apply to its collection.
pub(crate) fn enter(
    doc: &Document,
    ctx: &mut CommandContext,
) -> Result<Option<(Scope, Option<Document>)>, CommandError> {
    let Some((read, explained)) = read_of(doc) else {
        return Ok(None);
    };
    // Nothing to look for on a server with no settings: no hashing.
    if ctx.storage.is_none() || entries(ctx)?.is_empty() {
        return Ok(None);
    }
    let db = ctx.db_name.clone();
    let Some(hash) = crate::query_shape::hash(&db, read) else {
        return Ok(None);
    };
    let Some(entry) = entries(ctx)?
        .into_iter()
        .find(|e| e.get_str("queryShapeHash") == Ok(hash.as_str()))
    else {
        return Ok(None);
    };
    let settings = entry.get_document("settings").cloned().unwrap_or_default();
    if !explained && settings.get_bool("reject") == Ok(true) {
        return Err(error(411, "Query rejected by admin query settings"));
    }
    let name = read.keys().next().map(String::as_str).unwrap_or("");
    let coll = read.get_str(name).unwrap_or("").to_string();
    let rewritten = allowed_for(&settings, &db, &coll).map(|allowed| {
        let mut read = read.clone();
        read.remove("hint");
        // Only `$natural` can serve it: the scan, in its direction.
        if let Some(natural) = natural_only(ctx, &db, &coll, read_filter(&read), &allowed) {
            read.insert("hint", natural);
        }
        if explained {
            let mut outer = doc.clone();
            outer.insert("explain", read);
            outer
        } else {
            read
        }
    });
    let previous = ACTIVE.with(|a| a.borrow_mut().replace(Rc::new(settings)));
    Ok(Some((Scope { previous }, rewritten)))
}

fn read_filter(read: &Document) -> Option<&Document> {
    let key = if read.contains_key("distinct") {
        "query"
    } else {
        "filter"
    };
    read.get_document(key).ok()
}

/// The allowed indexes `settings` names for `db.coll`.
fn allowed_for(settings: &Document, db: &str, coll: &str) -> Option<Vec<Bson>> {
    settings
        .get_array("indexHints")
        .ok()?
        .iter()
        .filter_map(Bson::as_document)
        .find(|h| {
            h.get_document("ns")
                .is_ok_and(|n| n.get_str("db") == Ok(db) && n.get_str("coll") == Ok(coll))
        })
        .and_then(|h| h.get_array("allowedIndexes").ok().cloned())
}

/// `{$natural: <direction>}` when `$natural` is allowed and no allowed index
/// can serve the query.
fn natural_only(
    ctx: &CommandContext,
    db: &str,
    coll: &str,
    filter: Option<&Document>,
    allowed: &[Bson],
) -> Option<Document> {
    let natural = allowed
        .iter()
        .filter_map(Bson::as_document)
        .find(|d| d.contains_key("$natural"))?;
    let storage = ctx.storage.as_ref()?;
    let catalog = crate::explain::catalog_of_collection(storage.as_ref(), db, coll).ok()?;
    let restricted = crate::index_filters::restrict(catalog, allowed);
    let empty = Document::new();
    if serves_any(&restricted, filter.unwrap_or(&empty), None) {
        return None;
    }
    Some(natural.clone())
}

/// Whether one of `indexes` can serve the query: its leading field is one
/// the query constrains or sorts on.
fn serves_any(
    indexes: &[std::sync::Arc<crate::explain::IndexInfo>],
    filter: &Document,
    sort: Option<&Document>,
) -> bool {
    let mut fields = Vec::new();
    predicate_paths(filter, &mut fields);
    if let Some(first) = sort.and_then(|s| s.keys().next()) {
        fields.push(first.clone());
    }
    indexes.iter().any(|ix| {
        ix.key
            .keys()
            .next()
            .is_some_and(|lead| fields.iter().any(|f| f == lead))
    })
}

fn predicate_paths(filter: &Document, out: &mut Vec<String>) {
    for (k, v) in filter {
        if k == "$and" || k == "$or" {
            if let Bson::Array(branches) = v {
                for b in branches.iter().filter_map(Bson::as_document) {
                    predicate_paths(b, out);
                }
            }
        } else if !k.starts_with('$') {
            out.push(k.clone());
        }
    }
}

/// The settings of the read running on this thread.
pub(crate) fn active() -> Option<Rc<Document>> {
    ACTIVE.with(|a| a.borrow().clone())
}

/// What the running read's settings say about planning a read of
/// `db.coll`: `None` when it has no settings (index filters apply),
/// `Some(None)` when they name no indexes for it, `Some(Some(catalog))` the
/// indexes they leave the planner.
pub(crate) fn restricted_catalog(
    db: &str,
    coll: &str,
    filter: &Document,
    sort: Option<&Document>,
    catalog: &[std::sync::Arc<crate::explain::IndexInfo>],
) -> Option<Option<Vec<std::sync::Arc<crate::explain::IndexInfo>>>> {
    let settings = active()?;
    let Some(allowed) = allowed_for(&settings, db, coll) else {
        return Some(None);
    };
    let restricted = crate::index_filters::restrict(catalog.to_vec(), &allowed);
    let natural = allowed
        .iter()
        .filter_map(Bson::as_document)
        .any(|d| d.contains_key("$natural"));
    if serves_any(&restricted, filter, sort) || natural {
        Some(Some(restricted))
    } else {
        // Nothing allowed can serve it: planned as if without hints.
        Some(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_keeps_mongods_order_and_unsets_reject_false() {
        let old = doc! {"reject": true, "queryFramework": "classic"};
        assert_eq!(
            merged(Some(&old), &doc! {"comment": "c"}),
            doc! {"queryFramework": "classic", "reject": true, "comment": "c"}
        );
        assert_eq!(
            merged(Some(&old), &doc! {"reject": false}),
            doc! {"queryFramework": "classic"}
        );
    }

    #[test]
    fn hashes_parse_as_mongods() {
        assert_eq!(parse_hash(&"ab".repeat(32)).unwrap(), "AB".repeat(32));
        assert_eq!(parse_hash("abc").unwrap_err().code, 2);
        assert_eq!(parse_hash("AB").unwrap_err().code, 16);
    }

    #[test]
    fn id_hack_eligibility() {
        assert!(id_hack_eligible(
            &doc! {"find": "a", "filter": {"_id": 1}, "sort": {"a": 1}}
        ));
        assert!(id_hack_eligible(
            &doc! {"find": "a", "filter": {"_id": {"$eq": 1}}}
        ));
        assert!(id_hack_eligible(
            &doc! {"find": "a", "filter": {"_id": {"a": 1}}}
        ));
        assert!(!id_hack_eligible(
            &doc! {"find": "a", "filter": {"_id": {"$gt": 1}}}
        ));
        assert!(!id_hack_eligible(
            &doc! {"find": "a", "filter": {"_id": 1}, "skip": 1}
        ));
        assert!(!id_hack_eligible(
            &doc! {"distinct": "a", "query": {"_id": 1}}
        ));
    }
}

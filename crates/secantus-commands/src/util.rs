//! Small helpers shared across command handlers (field extraction, the byte
//! seam, and storage-error → reply mapping). Kept `pub(crate)`.

use bson::{doc, Bson, Document};

use secantus_core::collation::{Collation, Context as CollationContext};

use crate::{CommandError, StorageError};

/// Resolve a command-level `let` field into query vars. Seeds `$$NOW` (a Date
/// constant for the whole operation) then evaluates each `let` value as an
/// aggregation expression against an empty doc (so `{n: {$add: [1, 2]}}` binds
/// `$$n` to 3; scalars pass through). Mirrors `commands._resolve_let_vars`. A
/// value the expression engine can't evaluate is kept raw (best-effort).
pub(crate) fn resolve_let_vars(let_field: Option<&Bson>) -> Document {
    resolve_let_vars_at(let_field, None)
}

/// [`resolve_let_vars`] for a command run by `ctx`: on a replica-set member
/// `$$CLUSTER_TIME` is the cluster time the operation starts at, as on
/// mongod (8.2.11 sets a field to a Timestamp through it).
pub(crate) fn command_let_vars(let_field: Option<&Bson>, ctx: &crate::CommandContext) -> Document {
    resolve_let_vars_at(let_field, operation_cluster_time(ctx))
}

/// The cluster time an operation `ctx` runs starts at, on a replica-set
/// member: what `$$CLUSTER_TIME` reads. `None` on a standalone.
pub(crate) fn operation_cluster_time(ctx: &crate::CommandContext) -> Option<bson::Timestamp> {
    ctx.replica_set_name
        .as_ref()
        .and_then(|_| ctx.storage().ok())
        .map(|s| s.peek_cluster_time())
}

/// mongod's refusal of `$$CLUSTER_TIME` on a standalone, when one of
/// `values` (a pipeline's stages, an update) reads it.
pub(crate) fn cluster_time_unavailable(standalone: bool, values: &[Bson]) -> Option<CommandError> {
    fn reads(v: &Bson) -> bool {
        match v {
            Bson::String(s) => s == "$$CLUSTER_TIME" || s.starts_with("$$CLUSTER_TIME."),
            Bson::Document(d) => d.values().any(reads),
            Bson::Array(a) => a.iter().any(reads),
            _ => false,
        }
    }
    (standalone && values.iter().any(reads)).then(|| {
        CommandError::new(
            10071200,
            "Location10071200",
            "system variable $$CLUSTER_TIME is not available in standalone mode",
        )
    })
}

pub(crate) fn resolve_let_vars_at(
    let_field: Option<&Bson>,
    cluster_time: Option<bson::Timestamp>,
) -> Document {
    let mut vars = Document::new();
    vars.insert("NOW", bson::DateTime::now());
    if let Some(ts) = cluster_time {
        vars.insert("CLUSTER_TIME", ts);
    }
    if let Some(Bson::Document(d)) = let_field {
        for (name, value) in d {
            let resolved = secantus_core::expressions::evaluate(&Document::new(), value, &vars)
                .unwrap_or_else(|_| value.clone());
            vars.insert(name.clone(), resolved);
        }
    }
    vars
}

/// [`resolve_let_vars`], refusing a `let` expression that reads a field: the
/// variables are bound before any document is looked at.
pub(crate) fn checked_let_vars(let_field: Option<&Bson>) -> Result<Document, CommandError> {
    fn reads_a_field(v: &Bson) -> bool {
        match v {
            Bson::String(s) => s.starts_with('$') && !s.starts_with("$$"),
            Bson::Document(d) => !d.contains_key("$literal") && d.values().any(reads_a_field),
            Bson::Array(a) => a.iter().any(reads_a_field),
            _ => false,
        }
    }
    if let Some(Bson::Document(d)) = let_field {
        if d.values().any(reads_a_field) {
            return Err(CommandError::new(
                4890500,
                "Location4890500",
                "Command let Expression tried to access a field, but this is not allowed \
                 becauseCommand let Expressions run before the query examines any documents.",
            ));
        }
    }
    Ok(resolve_let_vars(let_field))
}

/// What a command said about collation.
pub(crate) enum Requested {
    /// No `collation` field, or an empty one: the namespace's default applies.
    Absent,
    /// `{locale: "simple"}`: binary comparison, whatever the default is.
    Simple,
    Given(Collation),
}

impl Requested {
    /// The collation named, if the command named one at all.
    pub(crate) fn named(self) -> Option<Option<Collation>> {
        match self {
            Requested::Absent => None,
            Requested::Simple => Some(None),
            Requested::Given(c) => Some(Some(c)),
        }
    }
}

/// A refused collation document as the command error mongod sends.
pub(crate) fn collation_error(e: secantus_core::collation::SpecError) -> CommandError {
    CommandError::new(e.code, error_code_name(e.code), e.message)
}

/// Read and VALIDATE the `collation` field of a command or of one write
/// statement, with mongod's checks. The field's type is the caller's to check
/// first (each command words that refusal its own way); a value that is not a
/// document is read here as absent.
pub(crate) fn requested_collation(doc: &Document) -> Result<Requested, CommandError> {
    match doc.get("collation").and_then(Bson::as_document) {
        None => Ok(Requested::Absent),
        Some(d) if d.is_empty() => Ok(Requested::Absent),
        Some(d) => {
            match secantus_core::collation::parse_strict(d, CollationContext::COMMAND)
                .map_err(collation_error)?
            {
                Some(c) => Ok(Requested::Given(c)),
                None => Ok(Requested::Simple),
            }
        }
    }
}

/// The default collation of a collection or view: what `create` was given.
/// `None` for a namespace that does not exist, or has none.
pub(crate) fn default_collation(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
) -> Option<Collation> {
    storage
        .get_collection_options(db, coll)
        .ok()?
        .get_document("collation")
        .ok()
        .and_then(secantus_core::collation::parse)
}

/// The collation a command on `db.coll` runs under: the one it names, else the
/// namespace's default. Refuses a malformed collation, and a collation that
/// is not a VIEW's own (`167`): a view reads in its default and no other.
pub(crate) fn effective_collation(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    doc: &Document,
) -> Result<Option<Collation>, CommandError> {
    let requested = requested_collation(doc)?;
    let options = storage.get_collection_options(db, coll).unwrap_or_default();
    let default = options
        .get_document("collation")
        .ok()
        .and_then(secantus_core::collation::parse);
    let Some(named) = requested.named() else {
        return Ok(default);
    };
    if options.contains_key("viewOn") {
        let same = match (&named, &default) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        };
        if !same {
            return Err(CommandError::new(
                167,
                "OptionNotSupportedOnView",
                "Cannot override a view's default collation",
            ));
        }
    }
    Ok(named)
}

/// The collection name from a string-valued command field (`doc[cmd]`).
pub(crate) fn coll_arg(doc: &Document, cmd: &str) -> Result<String, CommandError> {
    match doc.get(cmd) {
        Some(Bson::String(s)) => Ok(s.clone()),
        _ => Err(CommandError::new(
            2,
            "BadValue",
            format!("{cmd} command requires a string collection name"),
        )),
    }
}

/// A document-valued field, or an empty document when absent / wrong type
/// (mirrors Python's `spec.get("q", {})` / `doc.get("query") or {}`).
/// A `hint` must name an index that exists: refused, when it does not, in
/// mongod's planner's words, which print the query the command made
/// (`query`, measured per command against mongod 8.2.11, 2026-10-20).
///
/// `find` / `count` / `aggregate` also route their hint through the storage
/// layer, which rejects an unknown one; `update` / `delete` /
/// `findAndModify` never passed theirs anywhere, so `hint: "nosuch"` was
/// accepted and the write ran unhinted while mongod fails the command.
/// Validating without planning off it is result-equivalent: a VALID hint
/// selects an index, never a different set of documents.
pub(crate) fn validate_hint(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    hint: Option<&Bson>,
    query: &crate::explain::CanonicalQuery,
) -> Result<(), CommandError> {
    if hint_is_known(storage, db, coll, hint) {
        return Ok(());
    }
    Err(CommandError::new(
        2,
        "BadValue",
        query.planner_error("hint provided does not correspond to an existing index"),
    ))
}

/// Whether `hint` names an index of `db.coll` (or is no hint at all).
pub(crate) fn hint_is_known(
    storage: &dyn crate::Storage,
    db: &str,
    coll: &str,
    hint: Option<&Bson>,
) -> bool {
    let hint = match hint {
        None | Some(Bson::Null) => return true,
        // An empty document is no hint (measured 8.2.11, 2026-10-12).
        Some(Bson::Document(d)) if d.is_empty() => return true,
        Some(h) => h,
    };
    // A hint is validated during query PLANNING, and there is nothing to plan
    // against when the collection does not exist — so mongod accepts any hint
    // there. Measured 8.2.11 (2026-09-28) across all seven commands that take
    // a hint: `ok` on a missing namespace, BadValue the moment the collection
    // exists, empty or not. The storage layer's `resolve_hint` has the same
    // guard.
    if !storage.collection_exists(db, coll).unwrap_or(true) {
        return true;
    }
    // A hidden index is no index to a hint, on a write as on a read.
    let indexes: Vec<Document> = storage
        .list_indexes(db, coll)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.get_bool("hidden") != Ok(true))
        .collect();
    match hint {
        Bson::String(s) => {
            // `"$natural"` is deliberately absent: mongod takes only the
            // document form and rejects the string (probed 8.2.11,
            // 2026-08-31). The `Bson::Document` arm below still accepts
            // `{$natural: ...}`.
            s == "_id_"
                || indexes
                    .iter()
                    .any(|i| i.get_str("name").map(|n| n == s).unwrap_or(false))
        }
        Bson::Document(spec) => {
            (spec.len() == 1 && (spec.contains_key("$natural") || spec.contains_key("_id")))
                || indexes
                    .iter()
                    .any(|i| i.get_document("key").map(|k| k == spec).unwrap_or(false))
        }
        // The type error is raised by `argtypes::require_hint`.
        _ => true,
    }
}

pub(crate) fn doc_field(doc: &Document, key: &str) -> Document {
    doc.get(key)
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default()
}

/// A bool-valued field with a default (truthy like Python's `bool(...)`).
pub(crate) fn bool_field(doc: &Document, key: &str, default: bool) -> bool {
    doc.get(key).and_then(Bson::as_bool).unwrap_or(default)
}

/// Coerce any BSON number to `i64` (for `skip` / `limit` / `batchSize` / `index`).
pub(crate) fn as_i64(b: &Bson) -> Option<i64> {
    match b {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        // Drivers may encode a numeric command option as any BSON number —
        // mongo-c-driver's `batchsize_override_decimal128` sends `batchSize` as a
        // Decimal128. There's no direct Decimal128→int, so parse its decimal
        // string (i64 first, then f64 for a fractional form). Mirrors
        // `commands._coerce_command_int`.
        Bson::Decimal128(d) => {
            let s = d.to_string();
            s.parse::<i64>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
        }
        _ => None,
    }
}

/// Decode a batch of encoded documents into reply `Bson::Document`s (for
/// `firstBatch` / `nextBatch`).
pub(crate) fn docs_to_bson(batch: Vec<Vec<u8>>) -> Result<Vec<Bson>, CommandError> {
    batch
        .iter()
        .map(|b| {
            secantus_core::bsondoc::from_bytes(b.as_slice())
                .map(Bson::Document)
                .map_err(|e| {
                    CommandError::new(
                        1,
                        "InternalError",
                        format!("failed to decode document: {e}"),
                    )
                })
        })
        .collect()
}

/// Decode encoded documents into owned `Document`s (for handing to the
/// `secantus-core` engines, e.g. the aggregation pipeline).
pub(crate) fn decode_docs(batch: Vec<Vec<u8>>) -> Result<Vec<Document>, CommandError> {
    batch
        .iter()
        .map(|b| {
            secantus_core::bsondoc::from_bytes(b.as_slice()).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to decode document: {e}"),
                )
            })
        })
        .collect()
}

fn minimal_decode_err(e: impl std::fmt::Display) -> CommandError {
    CommandError::new(
        1,
        "InternalError",
        format!("failed to decode document: {e}"),
    )
}

/// Decode only the given top-level `fields` from each stored blob, skipping the
/// bytes of every other field. Used to feed a `$group` whose field references
/// have been collected by `secantus_core::referenced_top_level_fields`: a wide
/// document ahead of a `$group` that reads two fields is materialized as a
/// two-field document, not fully. The per-field decode goes through the same
/// `Bson` conversion `decode_docs` would apply, so the group sees byte-identical
/// values for the fields it reads (and an absent field is absent either way).
pub(crate) fn decode_docs_minimal(
    batch: Vec<Vec<u8>>,
    fields: &std::collections::BTreeSet<String>,
) -> Result<Vec<Document>, CommandError> {
    batch
        .iter()
        .map(|b| {
            secantus_core::bsondoc::fields(b, |key| fields.contains(key))
                .map_err(minimal_decode_err)
        })
        .collect()
}

/// Encode owned `Document`s back to bytes (for the cursor / storage seam).
/// [`encode_docs`] without taking ownership.
pub(crate) fn encode_docs_ref(docs: &[Document]) -> Result<Vec<Vec<u8>>, CommandError> {
    docs.iter()
        .map(|d| {
            let mut v = Vec::new();
            crate::bsondoc::write(d, &mut v).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to encode document: {e}"),
                )
            })?;
            Ok(v)
        })
        .collect()
}

pub(crate) fn encode_docs(docs: Vec<Document>) -> Result<Vec<Vec<u8>>, CommandError> {
    docs.iter()
        .map(|d| {
            let mut v = Vec::new();
            crate::bsondoc::write(d, &mut v).map_err(|e| {
                CommandError::new(
                    1,
                    "InternalError",
                    format!("failed to encode document: {e}"),
                )
            })?;
            Ok(v)
        })
        .collect()
}

/// mongod's wrapper for an EXECUTION-time update error.
///
/// mongod reports the update errors that depend on the stored document -- a
/// `$push` onto a non-array, an `$inc` past int64, an `_id` change -- as
/// `Plan executor error during <command> :: caused by :: <message>`, and leaves
/// the parse errors readable from the update spec alone (an unknown modifier, a
/// path conflict, a `$rename` onto itself) bare. Probed 8.2.11 (2026-09-06)
/// across ten wrapped and seven bare shapes, for both `update` and
/// `findAndModify`.
///
/// The wrapper names the COMMAND, so it cannot be baked into the engine's
/// message; the engine sets `exec` and the handler supplies the name. Mirrors
/// the two sites in `secantus.commands` that do the same on the Python server.
pub(crate) fn exec_wrapped(errmsg: String, exec: bool, command: &str) -> String {
    // With no command to name (a DDL path reporting a storage error) there
    // is no executor, so no wrapper.
    if exec && !command.is_empty() {
        format!("Plan executor error during {command} :: caused by :: {errmsg}")
    } else {
        errmsg
    }
}

/// Shape a per-operation `writeError` document from a pre-classified storage
/// error. `command` is the mongod command name for the executor wrapper (see
/// [`exec_wrapped`]) -- `"update"` or `"delete"`.
pub(crate) fn write_error(index: usize, err: StorageError, command: &str) -> Document {
    match err {
        StorageError::DuplicateKey(info) => {
            let errmsg = exec_wrapped(info.errmsg, info.exec, command);
            let mut e = doc! { "index": index as i32, "code": 11000, "errmsg": errmsg };
            if let Some(kp) = info.key_pattern {
                e.insert("keyPattern", kp);
            }
            if let Some(kv) = info.key_value {
                e.insert("keyValue", kv);
            }
            if let Some(extra) = info.extra {
                e.extend(extra);
            }
            e
        }
        StorageError::WriteError { code, errmsg, exec } => {
            let errmsg = exec_wrapped(errmsg, exec, command);
            doc! { "index": index as i32, "code": code, "errmsg": errmsg }
        }
        // Internal is handled by callers (command-level error); shouldn't reach
        // here, but degrade gracefully to a generic write error if it does.
        // The handler attaches `errInfo` (it holds the validator).
        StorageError::ValidationFailure(_) => doc! {
            "index": index as i32,
            "code": 121,
            "errmsg": exec_wrapped("Document failed validation".into(), true, command),
        },
        StorageError::Internal(msg) => {
            doc! { "index": index as i32, "code": 1, "errmsg": msg }
        }
        // WriteConflict is routed command-level by the handlers; degrade
        // gracefully to a per-op 112 if it ever reaches here.
        StorageError::WriteConflict => {
            doc! { "index": index as i32, "code": 112, "errmsg": "WriteConflict" }
        }
    }
}

/// A READ command's execution-time refusal, under mongod's namespace-naming
/// wrapper.
///
/// mongod has two executor wrappers and they are not interchangeable: an
/// update-side failure gets `Plan executor error during <command> :: caused by
/// ::` ([`exec_wrapped`]) while a read-side one gets `Executor error during
/// <command> command: <db>.<coll> :: caused by ::`. The ambiguous-sort-path
/// refusal (16746) is the read-side kind -- mongod discovers it per document --
/// and it reached the client under the update wrapper with an EMPTY command
/// name, because `command_error` assumed reads never carry an execution error.
pub(crate) fn read_exec_error(err: StorageError, command: &str, ns: &str) -> CommandError {
    match err {
        StorageError::WriteError {
            code,
            errmsg,
            exec: true,
        } => CommandError::new(
            code,
            // `error_code_name`, NOT `code_name_for`: the latter returns the
            // bare "Location" SENTINEL for a code with no symbolic name, and
            // emitting that verbatim put `codeName: "Location"` on the wire
            // where mongod says `Location<n>`. Found 2026-09-29 when 6108304
            // became the first unnamed code to travel this path under a
            // comparison that looked at codeName and not just code.
            error_code_name(code),
            format!("Executor error during {command} command: {ns} :: caused by :: {errmsg}"),
        ),
        other => command_error(other),
    }
}

/// [`read_exec_error`] for the read commands whose wrapper names the
/// namespace with "on namespace" -- `count`, `distinct` and `aggregate`
/// (measured 8.2.11, 2026-10-16, for an error inside a query's `$expr`).
/// `aggregate` also puts a constant's optimizer failure under its own
/// `Failed to optimize pipeline` wrapper.
pub(crate) fn read_error_on_namespace(err: StorageError, command: &str, ns: &str) -> CommandError {
    match err {
        StorageError::WriteError {
            code,
            errmsg,
            exec: true,
        } => CommandError::new(
            code,
            error_code_name(code),
            format!("Executor error during {command} command on namespace: {ns} :: caused by :: {errmsg}"),
        ),
        StorageError::WriteError {
            code,
            errmsg,
            exec: false,
        } if command == "aggregate" && errmsg.starts_with("Failed to optimize expression") => {
            CommandError::new(
                code,
                error_code_name(code),
                format!("Failed to optimize pipeline :: caused by :: {errmsg}"),
            )
        }
        other => command_error(other),
    }
}

/// Map a storage error to a command-level `CommandError` (for non-batch
/// commands like `count` / `find`). Read commands never carry an
/// execution-time update error, so this drops the flag; `findAndModify`, which
/// does, goes through [`command_error_during`].
/// Whether `reply` refuses a command that did not parse: mongod never ran
/// it, so it is no statement of a transaction and no run of the command.
pub(crate) fn did_not_parse(reply: &Document) -> bool {
    reply.get_f64("ok").unwrap_or(0.0) != 1.0
        && (matches!(reply.get_i32("code").unwrap_or(0), 16 | 40414 | 40415)
            || reply
                .get_str("errmsg")
                .is_ok_and(|m| m.starts_with("BSON field '") || m.starts_with("Expected field ")))
}

pub(crate) fn command_error(err: StorageError) -> CommandError {
    command_error_during(err, "")
}

/// [`command_error`] with the command name for mongod's executor wrapper --
/// `findAndModify` is the one non-batch command that applies an update.
pub(crate) fn command_error_during(err: StorageError, command: &str) -> CommandError {
    match err {
        StorageError::Internal(msg) => CommandError::new(1, "InternalError", msg),
        StorageError::WriteError { code, errmsg, exec } => CommandError::new(
            code,
            // See the note in `read_exec_error`: the bare "Location" sentinel
            // must be formatted into `Location<n>` before it reaches a client.
            error_code_name(code),
            exec_wrapped(errmsg, exec, command),
        ),
        // With the key it collided on, as mongod's reply has it.
        StorageError::DuplicateKey(info) => {
            let mut extra = Document::new();
            if let Some(pattern) = info.key_pattern {
                extra.insert("keyPattern", pattern);
            }
            if let Some(value) = info.key_value {
                extra.insert("keyValue", value);
            }
            let e = CommandError::new(11000, "DuplicateKey", info.errmsg);
            if extra.is_empty() {
                e
            } else {
                e.with_extra(extra)
            }
        }
        StorageError::ValidationFailure(_) => CommandError::new(
            121,
            "DocumentValidationFailure",
            exec_wrapped("Document failed validation".into(), true, command),
        ),
        StorageError::WriteConflict => CommandError::new(
            112,
            "WriteConflict",
            "Caused by :: Write conflict during plan execution and yielding is disabled. :: \
             Please retry your operation or multi-document transaction.",
        ),
    }
}

/// mongod's name for an error code: the real one where it has a name, and
/// `Location<n>` otherwise. The parse-time expression checks answer a mix of
/// both -- 16020 and 17276 are `Location`s, but 9 is `FailedToParse`, and
/// naming it `Location9` is a divergence the differential gate catches.
pub(crate) fn error_code_name(code: i32) -> String {
    match code_name_for(code) {
        "Location" => crate::failpoints::fail_code_name(code),
        name => name.to_string(),
    }
}

fn code_name_for(code: i32) -> &'static str {
    match code {
        2 => "BadValue",
        40413 => "IDLDuplicateField",
        40414 => "IDLFailedToParse",
        40415 => "IDLUnknownField",
        // A `maxTimeMS` budget spent mid-operation. Reaches here via the
        // storage layer's MaxTimeExpired -> WriteError { code: 50 } mapping,
        // and without the entry would render the generic `Location50`.
        50 => "MaxTimeMSExpired",
        // `$range`'s memory estimate over the 100 MiB expression limit.
        146 => "ExceededMemoryLimit",
        9 => "FailedToParse",
        67 => "CannotCreateIndex",
        85 => "IndexOptionsConflict",
        86 => "IndexKeySpecsConflict",
        112 => "WriteConflict",
        121 => "DocumentValidationFailure",
        10334 => "BSONObjectTooLarge",
        11000 => "DuplicateKey",
        66 => "ImmutableField",
        // Every unknown-expression error carries this, and a table miss renders
        // the generic `Location168`. Measured 8.2.11 (2026-09-17) across 13
        // shapes in `tools/probes/unknown_expression_errors.py`, where it was
        // the only remaining difference on nine of them.
        168 => "InvalidPipelineOperator",
        _ => "Location",
    }
}

/// Whether a BSON value reads as true where mongod takes a bool or a number.
pub(crate) fn truthy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => *b,
        Bson::Null => false,
        other => as_i64(other).is_some_and(|n| n != 0),
    }
}

/// A value where mongod takes a bool or any number and nothing else: `Some`
/// of how it reads, `None` for a value of another type.
pub(crate) fn truthy_number_or_bool(v: &Bson) -> Option<bool> {
    match v {
        Bson::Boolean(b) => Some(*b),
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => Some(match v {
            Bson::Double(d) => *d != 0.0,
            Bson::Decimal128(d) => d.to_string().parse::<f64>().is_ok_and(|f| f != 0.0),
            other => as_i64(other).is_some_and(|n| n != 0),
        }),
        _ => None,
    }
}

/// A numeric field of a reply, 0 when absent.
pub(crate) fn as_i64_field(doc: &Document, field: &str) -> i64 {
    doc.get(field).and_then(as_i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::as_i64;
    use bson::Bson;
    use std::str::FromStr;

    /// mongod wraps an EXECUTION-time update error and leaves a parse error
    /// bare, and the wrapper names the command. Probed 8.2.11 (2026-09-06).
    #[test]
    fn exec_wrapped_applies_mongods_executor_prefix() {
        assert_eq!(
            super::exec_wrapped(
                "Cannot apply $pull to a non-array value".into(),
                true,
                "update"
            ),
            "Plan executor error during update :: caused by :: \
             Cannot apply $pull to a non-array value"
        );
        // The command name is interpolated -- `findAndModify` reports its own.
        assert_eq!(
            super::exec_wrapped("boom".into(), true, "findAndModify"),
            "Plan executor error during findAndModify :: caused by :: boom"
        );
        // A parse error stays exactly as the engine wrote it.
        assert_eq!(
            super::exec_wrapped("An empty update path is not valid.".into(), false, "update"),
            "An empty update path is not valid."
        );
    }

    #[test]
    fn as_i64_coerces_numeric_types_incl_decimal128() {
        assert_eq!(as_i64(&Bson::Int32(2)), Some(2));
        assert_eq!(as_i64(&Bson::Int64(2)), Some(2));
        assert_eq!(as_i64(&Bson::Double(2.0)), Some(2));
        // Decimal128 batchSize (mongo-c-driver batchsize_override_decimal128).
        let d = Bson::Decimal128(bson::Decimal128::from_str("2").unwrap());
        assert_eq!(as_i64(&d), Some(2));
        // A non-number is rejected.
        assert_eq!(as_i64(&Bson::String("x".into())), None);
    }
}

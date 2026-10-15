//! Collection/index DDL + introspection + db-admin commands: `create` /
//! `collMod` / `explain` / `drop` / `listCollections` / `listIndexes` /
//! `createIndexes` / `dropIndexes` / `dropDatabase` / `renameCollection` /
//! `collStats` / `dbStats` / `serverStatus` / `validate` / `profile`.
//!
//! Ports of the corresponding `commands.py` handlers, scoped to the core paths.
//!
//! `create` persists recognised options (`validator` / `validationLevel` /
//! `validationAction` / `changeStreamPreAndPostImages` / `capped` / `size` /
//! `max`); `collMod` merges the same set into an existing collection (else
//! `NamespaceNotFound`). The `insert` handler enforces `validator` (code 121).
//!
//! `create` with `viewOn` + `pipeline` registers a read-only view; `count`
//! resolves through the view's pipeline and `listCollections` reports it as
//! `type: "view"` (readOnly, no `_id` index).
//!
//! **Deferred (documented so parity is honest):**
//! * capped-size enforcement.
//! * `validator` enforcement on `findAndModify` (insert / `update` / replace are
//!   enforced — the command layer reads the validator and the storage update
//!   checks the post-apply doc; code 121, bypassable via
//!   `bypassDocumentValidation`).
//! * `listIndexes` `NamespaceNotFound` on a missing collection (returns an empty
//!   cursor instead).
//! * `dropIndexes` by key-spec document (only by name / `"*"`).
//! * `serverStatus` reports a minimal subset; `collStats` / `dbStats` use
//!   `dataSize` for `storageSize` (no separate on-disk accounting).
//! * `writeConcern`, `_reject_oplog_rs_write`.

use bson::{doc, Bson, Document};

use crate::argtypes;
use crate::find::split_into_cursor;
use crate::util::{as_i64, bool_field, coll_arg, command_error, docs_to_bson, encode_docs};
use crate::{CommandContext, CommandError, HandlerResult, StorageError, DEFAULT_BATCH_SIZE};

/// Collection-option keys (from `create` / `collMod`) the Rust server persists.
/// `validator` + `validationLevel`/`validationAction` drive document validation;
/// `changeStreamPreAndPostImages` drives pre-image capture; `capped`/`size`/`max`
/// are reported in stats. (`collMod`'s `index` option, which modifies an
/// index's `expireAfterSeconds` and the rest, is handled in `coll_mod_inner`.)
const STORED_COLL_OPTIONS: [&str; 11] = [
    "validator",
    "validationLevel",
    "validationAction",
    "changeStreamPreAndPostImages",
    "capped",
    "size",
    "max",
    // Persisted so the storage layer can recognise a timeseries collection and
    // relax `_id` uniqueness (mongod buckets by time; `_id` is not a key).
    "timeseries",
    // The collection's default collation — surfaced back in `listCollections`.
    "collation",
    // Round-tripped in `listCollections` so drivers see the options they set on
    // `create` (the rust driver's collection_management asserts both).
    "storageEngine",
    "indexOptionDefaults",
];

/// The largest capped-collection `size` mongod accepts (1 PB).
const CAPPED_SIZE_MAX: i64 = 1 << 50;
/// What mongod stores for a capped `max` that means "no document limit".
pub(crate) const CAPPED_MAX_UNLIMITED: i64 = i32::MAX as i64;

/// An integer as mongod reports it: int32 when it fits, int64 otherwise.
pub(crate) fn int_bson(v: i64) -> Bson {
    match i32::try_from(v) {
        Ok(n) => Bson::Int32(n),
        Err(_) => Bson::Int64(v),
    }
}

/// A capped collection's byte bound, as `create.size` / `collMod.cappedSize`
/// take it (`field` names it in the error). mongod reads any number as a
/// 64-bit integer -- a fraction is dropped, NaN is 0, an out-of-range double
/// saturates -- and then requires 1 to 1 PB. Measured on 8.2.11.
fn capped_size(field: &str, v: &Bson) -> Result<i64, CommandError> {
    let n = as_i64(v).unwrap_or(0);
    if n < 1 {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("BSON field '{field}' value must be >= 1, actual value '{n}'"),
        ));
    }
    if n > CAPPED_SIZE_MAX {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("BSON field '{field}' value must be <= {CAPPED_SIZE_MAX}, actual value '{n}'"),
        ));
    }
    Ok(n)
}

/// A capped collection's document bound (`create.max` / `collMod.cappedMax`).
/// Zero or less means no limit, which mongod stores as 2147483647; 2^31 or
/// more is refused. Measured on 8.2.11.
fn capped_max(field: &str, v: &Bson) -> Result<i64, CommandError> {
    let n = as_i64(v).unwrap_or(0);
    if n > CAPPED_MAX_UNLIMITED {
        return Err(CommandError::new(
            2,
            "BadValue",
            format!("BSON field '{field}' value must be < 2147483648, actual value '{n}'"),
        ));
    }
    Ok(if n <= 0 { CAPPED_MAX_UNLIMITED } else { n })
}

/// The subset of a command doc that maps to persisted collection options.
fn collection_option_subset(doc: &Document) -> Document {
    let mut out = Document::new();
    for k in STORED_COLL_OPTIONS {
        if let Some(v) = doc.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    out
}

/// Why mongod would refuse to parse `validator` as a collection validator:
/// an invalid `$jsonSchema` (9 / 14 / 2, the same check `find` runs) or an
/// unknown query operator (2 `unknown operator: $x`). Measured 8.2.11,
/// 2026-10-06 -- both used to be accepted and stored.
fn validator_problem(v: &Document) -> Option<CommandError> {
    if let Some(e) = operator_not_allowed_in_validator(v) {
        return Some(e);
    }
    if let Some((code, name, msg)) = crate::find::json_schema_error_in_filter(v) {
        return Some(CommandError::new(code, name, msg));
    }
    secantus_core::query::first_unknown_operator(v)
        .map(|op| CommandError::new(2, "BadValue", format!("unknown operator: {op}")))
}

/// The operators a validator may not hold, anywhere in it: `$where`, `$text`
/// and the sorting geo operators. They were accepted and stored.
fn operator_not_allowed_in_validator(v: &Document) -> Option<CommandError> {
    for (key, value) in v {
        match key.as_str() {
            "$where" | "$text" => {
                return Some(CommandError::new(
                    2,
                    "BadValue",
                    format!("{key} is not allowed in this context"),
                ))
            }
            "$near" | "$nearSphere" | "$geoNear" => {
                return Some(CommandError::new(
                    5626500,
                    "Location5626500",
                    "$geoNear, $near, and $nearSphere are not allowed in this context, as these \
                     operators require sorting geospatial data. If you do not need sort, consider \
                     using $geoWithin instead. Check out \
                     https://dochub.mongodb.org/core/near-sort-operation and \
                     https://dochub.mongodb.org/core/nearSphere-sort-operationfor more details.",
                ))
            }
            _ => {}
        }
        let nested = match value {
            Bson::Document(d) => operator_not_allowed_in_validator(d),
            Bson::Array(a) => a
                .iter()
                .filter_map(Bson::as_document)
                .find_map(operator_not_allowed_in_validator),
            _ => None,
        };
        if nested.is_some() {
            return nested;
        }
    }
    None
}

/// `validationLevel` / `validationAction` take a fixed set of words; anything
/// else was stored as given and then silently read as the default.
fn validation_enum_problem(doc: &Document, command: &str) -> Option<CommandError> {
    for (field, allowed) in [
        ("validationLevel", &["off", "strict", "moderate"][..]),
        ("validationAction", &["error", "warn", "errorAndLog"][..]),
    ] {
        if let Some(Bson::String(v)) = doc.get(field) {
            if !allowed.contains(&v.as_str()) {
                return Some(CommandError::new(
                    2,
                    "BadValue",
                    format!(
                        "Enumeration value '{v}' for field '{command}.{field}' is not a valid value."
                    ),
                ));
            }
        }
    }
    None
}

/// `create` — create a collection, persisting recognised options.
pub fn create(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // Before the namespace checks: mongod parses the command before executing it,
    // so a wrong-typed option on a MISSING collection is still the type error.
    argtypes::require_object(doc, "storageEngine", "create.storageEngine")?;
    argtypes::require_object(doc, "validator", "create.validator")?;
    argtypes::require_object(doc, "timeseries", "create.timeseries")?;
    argtypes::require_object(doc, "collation", "create.collation")?;
    crate::ddl_checks::create(doc)?;
    // `capped` is bool-OR-number here; `size` / `max` are plain numeric. A null
    // in any of the three is ACCEPTED as absent and then answers the semantic
    // error ("the 'size' field is required when 'capped' is true"), not a type
    // error — which is why these use the null-tolerant validators.
    argtypes::require_bool_or_number(doc, "capped", "create.capped")?;
    argtypes::require_number(doc, "size", "create.size")?;
    argtypes::require_number(doc, "max", "create.max")?;
    // Past the type checks, two SEMANTIC rules mongod applies to the same three
    // options. A null reaches here (null means absent to the validators above),
    // which is why `capped: null, size: 4096` lands on the second one.
    let capped_true = matches!(doc.get("capped"), Some(Bson::Boolean(true)))
        || matches!(doc.get("capped"), Some(Bson::Int32(n)) if *n != 0)
        || matches!(doc.get("capped"), Some(Bson::Int64(n)) if *n != 0)
        || matches!(doc.get("capped"), Some(Bson::Double(d)) if *d != 0.0)
        || matches!(doc.get("capped"), Some(d @ Bson::Decimal128(_)) if as_i64(d) != Some(0));
    let has = |f: &str| !matches!(doc.get(f), None | Some(Bson::Null));
    // The RANGE of `size` and `max` is part of parsing the command, so it is
    // checked before the two rules below and whether or not `capped` is set:
    // `{create: "c", size: 0}` is the range error, not "needs to be true".
    let size = match doc.get("size") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(capped_size("size", v)?),
    };
    let max = match doc.get("max") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(capped_max("max", v)?),
    };
    if capped_true && !has("size") {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "the 'size' field is required when 'capped' is true",
        ));
    }
    if !capped_true && (has("size") || has("max")) {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "the 'capped' field needs to be true when either the 'size' or 'max' fields \
             are present",
        ));
    }
    let coll = coll_arg(doc, "create")?;
    if let Some(e) = invalid_collection_name(&ctx.db_name, &coll) {
        return Ok(e.into_reply());
    }
    if let Some(e) = crate::ddl_checks::namespace_too_long(&ctx.db_name, &coll) {
        return Ok(e.into_reply());
    }
    if let Some(e) = validation_enum_problem(doc, "create") {
        return Ok(e.into_reply());
    }
    // A time-series definition is checked whole, with the options it cannot
    // be combined with, before anything is created.
    let ns = format!("{}.{coll}", ctx.db_name);
    let ts_def = crate::timeseries::definition(doc, &ns)?;
    if ts_def.is_none() && coll.starts_with("system.buckets.") {
        return Ok(CommandError::new(
            20,
            "IllegalOperation",
            "Creation of a timeseries bucket collection without timeseries options is not allowed",
        )
        .into_reply());
    }
    let ts_expire = match doc.get("expireAfterSeconds") {
        Some(v) if ts_def.is_some() && !matches!(v, Bson::Null) => {
            Some(crate::timeseries::expire_after("create", v)?)
        }
        _ => None,
    };
    // A view's definition is checked when it is created, not when it is first
    // read: a pipeline that is not an array, an unknown or write stage, an
    // empty `viewOn` and a cycle were all accepted and stored.
    match doc.get("viewOn") {
        None | Some(Bson::Null | Bson::String(_)) => {}
        Some(v) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'create.viewOn' is the wrong type '{}', expected type 'string'",
                    secantus_core::query::bson_type_name(v)
                ),
            ))
        }
    }
    let view_pipeline = match doc.get("pipeline") {
        None | Some(Bson::Null) => None,
        Some(Bson::Array(p)) => Some(p.as_slice()),
        Some(v) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'create.pipeline' is the wrong type '{}', expected type 'array'",
                    secantus_core::query::bson_type_name(v)
                ),
            ))
        }
    };
    match doc.get("viewOn") {
        // A collection of that name answers "exists" before the definition
        // is looked at.
        Some(Bson::String(_))
            if ctx
                .storage()?
                .collection_exists(&ctx.db_name, &coll)
                .unwrap_or(false)
                && !crate::views::is_view(ctx.storage()?, &ctx.db_name, &coll) => {}
        Some(Bson::String(view_on)) => {
            let storage = ctx.storage()?;
            if let Some(problem) = crate::views::definition_problem(
                storage,
                &ctx.db_name,
                &coll,
                view_on,
                view_pipeline,
            ) {
                return Ok(problem.into_reply());
            }
        }
        _ if view_pipeline.is_some() => {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "'pipeline' requires 'viewOn' to also be specified",
            )
            .into_reply())
        }
        _ => {}
    }
    if let Some(unknown) = first_unknown_field(doc, CREATE_KNOWN_OPTIONS) {
        return Ok(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'create.{unknown}' is an unknown field."),
        )
        .into_reply());
    }
    // Build the options up front so they ride the `create` oplog entry (carried
    // by create_collection_with_options) — that's what lets PITR replay
    // reconstruct capped / validator / … rather than seeing a bare create.
    let mut opts = collection_option_subset(doc);
    // An option given as null is an option not given.
    opts = opts
        .into_iter()
        .filter(|(_, v)| !matches!(v, Bson::Null))
        .collect();
    // The default collation is stored spelled out in full, and `simple` as no
    // collation, which is how mongod stores and compares them.
    if let Some(Bson::Document(given)) = doc.get("collation") {
        match secantus_core::collation::parse_strict(
            given,
            secantus_core::collation::Context::CREATE,
        )
        .map_err(crate::util::collation_error)?
        {
            Some(c) => opts.insert("collation", c.spec().clone()),
            None => opts.remove("collation"),
        };
    }
    // A view on a view reads through both, so the two must agree.
    if let Some(Bson::String(view_on)) = doc.get("viewOn") {
        let storage = ctx.storage()?;
        let target = storage
            .get_collection_options(&ctx.db_name, view_on)
            .unwrap_or_default();
        if target.contains_key("viewOn") && target.get("collation") != opts.get("collation") {
            return Ok(CommandError::new(
                167,
                "OptionNotSupportedOnView",
                format!(
                    "View {db}.{coll} has conflicting collation with view {db}.{view_on}",
                    db = ctx.db_name
                ),
            )
            .into_reply());
        }
    }
    // An empty validator is no validator: mongod stores nothing for it.
    if matches!(opts.get("validator"), Some(Bson::Document(v)) if v.is_empty()) {
        opts.remove("validator");
    }
    // A capped collection's options are stored as mongod reports them:
    // `capped: true` whatever number said so, and integer `size` / `max`
    // (pymongo sends `size` as a double, and it used to be echoed as one).
    opts.remove("capped");
    opts.remove("size");
    opts.remove("max");
    if capped_true {
        opts.insert("capped", true);
        if let Some(size) = size {
            opts.insert("size", int_bson(size));
        }
        if let Some(max) = max {
            opts.insert("max", int_bson(max));
        }
    }
    // `viewOn` + `pipeline` makes this a read-only view of another collection
    // (mongod 3.4+). Store the source and the pipeline (under `viewPipeline` so
    // it doesn't collide with an aggregate's `pipeline`); `listCollections`
    // surfaces it as `type: "view"` and `count` resolves through it.
    if let Some(Bson::String(view_on)) = doc.get("viewOn") {
        opts.insert("viewOn", view_on.clone());
        let pipeline = doc
            .get("pipeline")
            .and_then(Bson::as_array)
            .cloned()
            .unwrap_or_default();
        opts.insert("viewPipeline", Bson::Array(pipeline));
    }
    // A collection-level TTL belongs to a clustered or time-series collection.
    if has("expireAfterSeconds") && !has("clusteredIndex") && !has("timeseries") {
        return Ok(CommandError::new(
            72,
            "InvalidOptions",
            "'expireAfterSeconds' is only supported on time-series collections or when the \
             'clusteredIndex' option is specified",
        )
        .into_reply());
    }
    // `clusteredIndex` clusters the collection on `_id` — which is already
    // SecantusDB's doc-table layout (keyed by `_id`), so this is metadata-only.
    // mongod allows it only on `{_id: 1}` with `unique: true`; normalise the
    // stored option (default name `_id_`, add `v: 2`) so listCollections /
    // listIndexes echo mongod's shape. Built before create so an invalid spec
    // rejects without leaving a half-created collection. Mirrors commands.py.
    if let Some(ci) = doc.get("clusteredIndex").and_then(Bson::as_document) {
        let key_ok = ci.get_document("key").is_ok_and(|k| {
            k.len() == 1
                && k.get("_id").is_some_and(|v| {
                    matches!(v, Bson::Int32(1) | Bson::Int64(1)) || v.as_f64() == Some(1.0)
                })
        });
        if !key_ok {
            return Ok(CommandError::new(
                197,
                "InvalidIndexSpecificationOption",
                "The clusteredIndex option is only supported for key: {_id: 1}",
            )
            .into_reply());
        }
        if ci.get_bool("unique") != Ok(true) {
            return Ok(CommandError::new(
                5979700,
                "Location5979700",
                "The clusteredIndex option requires unique: true to be specified",
            )
            .into_reply());
        }
        let name = ci.get_str("name").unwrap_or("_id_").to_string();
        opts.insert(
            "clusteredIndex",
            doc! { "v": 2i32, "key": { "_id": 1i32 }, "name": name, "unique": true },
        );
    }
    // A clustered collection's expiry is one of its options.
    if opts.contains_key("clusteredIndex") {
        if let Some(seconds) = doc.get("expireAfterSeconds").and_then(as_i64) {
            opts.insert("expireAfterSeconds", Bson::Int64(seconds));
        }
    }
    // A view keeps what defines it and nothing else: mongod takes `capped`
    // or a validator on a view and stores neither.
    if opts.contains_key("viewOn") {
        opts = opts
            .into_iter()
            .filter(|(k, _)| matches!(k.as_str(), "viewOn" | "viewPipeline" | "collation"))
            .collect();
    }
    // The definition is stored as mongod lists it: defaults filled in, and
    // after every other option.
    opts.remove("timeseries");
    if let Some(def) = &ts_def {
        if let Some(seconds) = ts_expire {
            opts.insert("expireAfterSeconds", Bson::Int64(seconds));
        }
        opts.insert("timeseries", def.clone());
    }
    let storage = ctx.storage()?;
    // The validator is parsed as the collection is created, so an existing
    // collection still answers 48 first (measured 8.2.11, 2026-10-06).
    if let Some(Bson::Document(v)) = doc.get("validator") {
        if let Some(problem) = validator_problem(v) {
            let exists = storage
                .list_collections(&ctx.db_name)
                .map_err(command_error)?
                .iter()
                .any(|c| c == &coll);
            if !exists {
                return Ok(problem.into_reply());
            }
        }
    }
    let created = storage
        .create_collection_with_options(&ctx.db_name, &coll, &opts)
        .map_err(command_error)?;
    if !created {
        // mongod answers ok when the collection already exists with the SAME
        // options -- a bare `create` of an existing collection included -- and
        // 48 quoting the existing options otherwise (measured 8.2.11,
        // 2026-10-01). This was always 48.
        let mut existing = storage
            .get_collection_options(&ctx.db_name, &coll)
            .unwrap_or_default();
        // The stored options carry the collection's UUID; it is quoted
        // separately, and is never a requested option.
        existing.remove("uuid");
        if existing == opts {
            return Ok(doc! { "ok": 1.0 });
        }
        // A time-series collection on either side has its own wording.
        // A view in the way is described by what it reads, or by its pipeline
        // when it reads the same thing.
        if let (Ok(on), false) = (
            existing.get_str("viewOn"),
            existing.contains_key("timeseries"),
        ) {
            let db = &ctx.db_name;
            let wanted = doc.get_str("viewOn").unwrap_or("");
            let pipeline = |d: &Document, key: &str| {
                crate::ddl_checks::spaced_value(d.get(key).unwrap_or(&Bson::Array(vec![])))
            };
            let message = if wanted == on {
                format!(
                    "namespace {ns} already exists, but with pipeline {} rather than {}",
                    pipeline(&existing, "viewPipeline"),
                    pipeline(doc, "pipeline"),
                )
            } else {
                let other = format!("{db}.{wanted}");
                format!(
                    "namespace {ns} already exists, but is a view on {db}.{on} rather than {}",
                    other.trim_end_matches('.'),
                )
            };
            return Ok(CommandError::new(48, "NamespaceExists", message).into_reply());
        }
        let message = match (existing.contains_key("timeseries"), ts_def.is_some()) {
            (true, true) => Some(format!("A timeseries collection already exists. NS: {ns}")),
            (false, true) => Some(format!("Collection {ns} already exists.")),
            (true, false) => Some(format!(
                "namespace {ns} already exists, but is a view on {db}.system.buckets.{coll} \
                 rather than {db}",
                db = ctx.db_name
            )),
            (false, false) => None,
        };
        if let Some(message) = message {
            return Ok(CommandError::new(48, "NamespaceExists", message).into_reply());
        }
        let uuid = storage
            .collection_uuid(&ctx.db_name, &coll)
            .ok()
            .and_then(|b| uuid_text(&b))
            .unwrap_or_default();
        let mut shown = format!("uuid: UUID(\"{uuid}\")");
        for (k, v) in &existing {
            shown.push_str(&format!(", {k}: {}", argtypes::render_stage_value(v)));
        }
        return Ok(CommandError::new(
            48,
            "NamespaceExists",
            format!(
                "namespace {}.{coll} already exists, but with different options: {{ {shown} }}",
                ctx.db_name
            ),
        )
        .into_reply());
    }
    // A time-series collection with a meta field starts with an index on
    // the meta and time fields.
    if let Some((name, key)) = ts_def
        .as_ref()
        .and_then(|_| crate::timeseries::Fields::of(&opts))
        .and_then(|f| f.default_index())
    {
        let spec = doc! { "key": key.clone(), "name": name.clone() };
        storage
            .create_index(&ctx.db_name, &coll, &name, &key, &spec)
            .map_err(command_error)?;
    }
    Ok(doc! { "ok": 1.0 })
}

/// `collMod` — modify a collection's options (`validator` / `validationLevel` /
/// `validationAction` / `changeStreamPreAndPostImages`). Merges the recognised
/// options into the collection's stored blob. Errors `NamespaceNotFound` (26)
/// when the collection doesn't exist. (TTL-index `index` modification deferred.)
pub fn coll_mod(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    crate::ddl_checks::coll_mod(doc)?;
    // mongod's reply ends with `ok`, after what the command changed.
    let mut reply = coll_mod_inner(doc, ctx)?;
    if reply.get("ok").is_some_and(crate::util::truthy) {
        if let Some(ok) = reply.remove("ok") {
            reply.insert("ok", ok);
        }
    }
    Ok(reply)
}

fn coll_mod_inner(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    argtypes::require_object(doc, "index", "collMod.index")?;
    argtypes::require_object(doc, "validator", "collMod.validator")?;
    argtypes::require_object(
        doc,
        "changeStreamPreAndPostImages",
        "collMod.changeStreamPreAndPostImages",
    )?;
    argtypes::require_string(doc, "viewOn", "collMod.viewOn")?;
    if let Some(e) = validation_enum_problem(doc, "collMod") {
        return Err(e);
    }
    for field in ["cappedSize", "cappedMax"] {
        if let Some(v) = doc.get(field) {
            if !matches!(
                v,
                Bson::Null
                    | Bson::Int32(_)
                    | Bson::Int64(_)
                    | Bson::Double(_)
                    | Bson::Decimal128(_)
            ) {
                // Not `argtypes::require_number`: collMod lists the numeric
                // types in its own order.
                return Err(CommandError::new(
                    14,
                    "TypeMismatch",
                    format!(
                        "BSON field 'collMod.{field}' is the wrong type '{}', expected types \
                         '[double, int, long, decimal]'",
                        secantus_core::query::bson_type_name(v)
                    ),
                ));
            }
        }
    }
    let capped_size_arg = match doc.get("cappedSize") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(capped_size("cappedSize", v)?),
    };
    let capped_max_arg = match doc.get("cappedMax") {
        None | Some(Bson::Null) => None,
        Some(v) => Some(capped_max("cappedMax", v)?),
    };
    // An unknown field is refused, not ignored (measured 8.2.11, 2026-10-01).
    if let Some(unknown) = doc
        .keys()
        .skip(1)
        .find(|k| !COLLMOD_FIELDS.contains(&k.as_str()) && !crate::params::is_generic_arg(k))
    {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'collMod.{unknown}' is an unknown field."),
        ));
    }
    let coll = match doc.get("collMod").or_else(|| doc.get("collmod")) {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "collMod requires a string collection name",
            ))
        }
    };
    let storage = ctx.storage()?;
    let exists = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?
        .iter()
        .any(|c| c == &coll);
    if !exists {
        return Ok(CommandError::new(26, "NamespaceNotFound", "ns does not exist").into_reply());
    }
    {
        let options = storage
            .get_collection_options(&ctx.db_name, &coll)
            .unwrap_or_default();
        if let Some(refused) = crate::ddl_checks::coll_mod_on(doc, &options) {
            return Err(refused);
        }
    }
    if let Some(Bson::Document(v)) = doc.get("validator") {
        if let Some(mut problem) = validator_problem(v) {
            problem.errmsg = format!(
                "Parsing of collection validator failed :: caused by :: {}",
                problem.errmsg
            );
            return Ok(problem.into_reply());
        }
    }
    // `viewOn` / `pipeline` redefine a view and nothing else; a view takes
    // none of a collection's options. Until 2026-10-09 a view's `collMod` was
    // stored under the wrong key and changed nothing.
    let is_view = crate::views::is_view(storage, &ctx.db_name, &coll);
    let given = |f: &str| !matches!(doc.get(f), None | Some(Bson::Null));
    let new_pipeline = match doc.get("pipeline") {
        None | Some(Bson::Null) => None,
        Some(Bson::Array(p)) => Some(p.clone()),
        Some(v) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'collMod.pipeline' is the wrong type '{}', expected type 'array'",
                    secantus_core::query::bson_type_name(v)
                ),
            ))
        }
    };
    let new_view_on = doc.get_str("viewOn").ok().map(String::from);
    if is_view {
        const NOT_ON_A_VIEW: [&str; 5] = [
            "validator",
            "validationLevel",
            "validationAction",
            "index",
            "changeStreamPreAndPostImages",
        ];
        if let Some(option) = doc
            .keys()
            .find(|k| NOT_ON_A_VIEW.contains(&k.as_str()) && given(k))
        {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                format!("option not supported on a view: {option}"),
            )
            .into_reply());
        }
        if new_view_on.is_some() || new_pipeline.is_some() {
            let current = storage
                .get_collection_options(&ctx.db_name, &coll)
                .unwrap_or_default();
            let view_on = new_view_on
                .clone()
                .or_else(|| current.get_str("viewOn").ok().map(String::from))
                .unwrap_or_default();
            if let Some(problem) = crate::views::definition_problem(
                storage,
                &ctx.db_name,
                &coll,
                &view_on,
                new_pipeline.as_deref(),
            ) {
                return Ok(problem.into_reply());
            }
        }
    } else if let Some(option) = ["pipeline", "viewOn"].into_iter().find(|f| given(f)) {
        return Ok(CommandError::new(
            72,
            "InvalidOptions",
            format!("option only supported on a view: {option}"),
        )
        .into_reply());
    }
    // A time-series collection takes a wider bucket and a TTL, and none of
    // the validation options; `timeseries` belongs to no other collection.
    let stored_options = storage
        .get_collection_options(&ctx.db_name, &coll)
        .unwrap_or_default();
    let ts_now = stored_options.get_document("timeseries").ok();
    let mut ts_changes = Document::new();
    match ts_now {
        Some(current) => {
            const NOT_ON_TIME_SERIES: [&str; 4] = [
                "validator",
                "validationLevel",
                "validationAction",
                "changeStreamPreAndPostImages",
            ];
            if let Some(option) = doc
                .keys()
                .find(|k| NOT_ON_TIME_SERIES.contains(&k.as_str()) && given(k))
            {
                return Ok(CommandError::new(
                    72,
                    "InvalidOptions",
                    format!("option not supported on a time-series collection: {option}"),
                )
                .into_reply());
            }
            if let Some(Bson::Document(change)) = doc.get("timeseries") {
                ts_changes.insert("timeseries", crate::timeseries::modified(current, change)?);
            }
            match doc.get("expireAfterSeconds") {
                None => {}
                Some(Bson::String(s)) if s == "off" => {
                    ts_changes.insert("expireAfterSeconds", Bson::Null);
                }
                Some(Bson::String(s)) => {
                    return Ok(CommandError::new(
                        72,
                        "InvalidOptions",
                        format!(
                            "Invalid string value for the 'clusteredIndex::expireAfterSeconds' \
                             option. Got: '{s}'. Accepted value is 'off'"
                        ),
                    )
                    .into_reply());
                }
                Some(v) => {
                    let seconds = crate::timeseries::expire_after("collMod", v)?;
                    ts_changes.insert("expireAfterSeconds", Bson::Int64(seconds));
                }
            }
        }
        None if given("timeseries") => {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "option only supported on a time-series collection: timeseries",
            )
            .into_reply());
        }
        None => {}
    }
    let mut reply = doc! { "ok": 1.0 };
    // Index modification: `collMod {index: {keyPattern|name, prepareUnique|unique|expireAfterSeconds}}`.
    if let Some(Bson::Document(index_spec)) = doc.get("index") {
        // The fields `collMod.index` takes; anything else is refused before
        // the index is looked up (measured 8.2.11, 2026-10-10 -- `collation`
        // and `sparse` among them: neither can be changed on an index).
        const KNOWN: &[&str] = &[
            "name",
            "keyPattern",
            "expireAfterSeconds",
            "hidden",
            "unique",
            "prepareUnique",
            "forceNonUnique",
        ];
        if let Some(unknown) = index_spec.keys().find(|k| !KNOWN.contains(&k.as_str())) {
            return Ok(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("BSON field 'collMod.index.{unknown}' is an unknown field."),
            )
            .into_reply());
        }
        let indexes = storage
            .list_indexes(&ctx.db_name, &coll)
            .map_err(command_error)?;
        // Resolve the target index by name or key pattern.
        let target = if let Ok(want) = index_spec.get_str("name") {
            indexes
                .iter()
                .find(|ix| ix.get_str("name") == Ok(want))
                .cloned()
        } else if let Some(Bson::Document(want_key)) = index_spec.get("keyPattern") {
            indexes
                .iter()
                .find(|ix| {
                    ix.get_document("key")
                        .map(|k| key_patterns_eq(k, want_key))
                        .unwrap_or(false)
                })
                .cloned()
        } else {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "Must specify either index name or key pattern.",
            )
            .into_reply());
        };
        let Some(target) = target else {
            let wanted = match index_spec.get("name") {
                Some(Bson::String(n)) => n.clone(),
                _ => index_spec
                    .get("keyPattern")
                    .map(argtypes::render_stage_value)
                    .unwrap_or_default(),
            };
            return Ok(CommandError::new(
                27,
                "IndexNotFound",
                format!("cannot find index {wanted} for ns {}.{}", ctx.db_name, coll),
            )
            .into_reply());
        };
        if !["expireAfterSeconds", "hidden", "unique", "prepareUnique"]
            .iter()
            .any(|f| index_spec.contains_key(*f))
        {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "no expireAfterSeconds, hidden, unique, or prepareUnique field",
            )
            .into_reply());
        }
        let target_name = target.get_str("name").unwrap_or("").to_string();
        // `hidden` was accepted and ignored: the reply said ok and the index
        // stayed as it was. The reply names the change only when there is one.
        if let Some(hide) = index_spec.get("hidden").and_then(Bson::as_bool) {
            if target_name == "_id_" {
                return Ok(CommandError::new(2, "BadValue", "can't hide _id index").into_reply());
            }
            let hidden = target.get_bool("hidden").unwrap_or(false);
            if hidden != hide {
                reply.insert("hidden_old", hidden);
                reply.insert("hidden_new", hide);
                storage
                    .set_index_options(&ctx.db_name, &coll, &target_name, &doc! {"hidden": hide})
                    .map_err(command_error)?;
            }
        }
        if let Some(new_expiry) = index_spec.get("expireAfterSeconds") {
            // Checked as mongod checks it (measured 8.2.11, 2026-10-10). Every
            // one of these used to be stored: a string, a negative number, and
            // a TTL on a compound index, which then deleted documents.
            let invalid = |why: &str| CommandError::new(72, "InvalidOptions", why).into_reply();
            let seconds = match new_expiry {
                Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => {
                    as_i64(new_expiry).unwrap_or(0)
                }
                other => {
                    return Ok(CommandError::new(
                        14,
                        "TypeMismatch",
                        format!(
                            "BSON field 'collMod.index.expireAfterSeconds' is the wrong type \
                             '{}', expected types '[long, int, decimal, double]'",
                            secantus_core::query::bson_type_name(other)
                        ),
                    )
                    .into_reply())
                }
            };
            if seconds < 0 {
                return Ok(invalid(
                    "TTL index 'expireAfterSeconds' option cannot be less than 0",
                ));
            }
            let target_key = target.get_document("key").cloned().unwrap_or_default();
            if target_name == "_id_" {
                return Ok(invalid("the _id field does not support TTL indexes"));
            }
            if target_key.len() > 1 {
                return Ok(invalid(
                    "TTL indexes are single-field indexes, compound indexes do not support TTL",
                ));
            }
            // A value past int32 is stored, and reported, as int32's largest.
            let seconds = seconds.min(i64::from(i32::MAX));
            // Both as int64, and no `_old` for an index that had no TTL.
            if let Some(old) = target.get("expireAfterSeconds").and_then(as_i64) {
                reply.insert("expireAfterSeconds_old", old);
            }
            reply.insert("expireAfterSeconds_new", seconds);
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"expireAfterSeconds": seconds as i32},
                )
                .map_err(command_error)?;
        }
        // `prepareUnique` arms the index: new dup writes are rejected (11000)
        // while pre-existing duplicates are tolerated — the staging step before
        // a `unique: true` conversion.
        let already_unique = target.get_bool("unique").unwrap_or(false);
        let mut prepared = target.get_bool("prepareUnique").unwrap_or(false);
        if let Some(prep) = index_spec.get("prepareUnique").and_then(Bson::as_bool) {
            // The reply names the change only when there is one, and a unique
            // index has nothing left to prepare.
            if !already_unique && prep != prepared {
                reply.insert("prepareUnique_old", prepared);
                reply.insert("prepareUnique_new", prep);
                storage
                    .set_index_options(
                        &ctx.db_name,
                        &coll,
                        &target_name,
                        &doc! {"prepareUnique": prep},
                    )
                    .map_err(command_error)?;
                prepared = prep;
            }
        }
        if index_spec.get("unique").and_then(Bson::as_bool) == Some(false) {
            return Ok(
                CommandError::new(2, "BadValue", "Cannot make index non-unique").into_reply(),
            );
        }
        // The conversion is a second step: the index must already be refusing
        // new duplicates.
        if index_spec.get("unique").and_then(Bson::as_bool) == Some(true)
            && !already_unique
            && !prepared
        {
            return Ok(CommandError::new(
                72,
                "InvalidOptions",
                "Cannot make index unique with 'prepareUnique=false'. Run collMod to set it \
                 first.",
            )
            .into_reply());
        }
        // `unique: true` converts the index; if any docs already share a key the
        // conversion is refused with 359 and the offending `_id` groups reported
        // as `violations`. Mirrors commands.py::_coll_mod.
        if index_spec.get("unique").and_then(Bson::as_bool) == Some(true)
            && !target.get_bool("unique").unwrap_or(false)
        {
            let dups = storage
                .find_index_duplicates(&ctx.db_name, &coll, &target_name)
                .map_err(command_error)?;
            if !dups.is_empty() {
                let violations: Vec<Bson> = dups
                    .into_iter()
                    .map(|ids| Bson::Document(doc! {"ids": ids}))
                    .collect();
                let mut reply = CommandError::new(
                    359,
                    "CannotConvertIndexToUnique",
                    "Cannot convert the index to unique. Please resolve conflicting documents \
                     before running collMod again.",
                )
                .into_reply();
                reply.insert("violations", violations);
                return Ok(reply);
            }
            // A dry run stops at having found nothing in the way.
            if doc.get("dryRun").is_some_and(crate::util::truthy) {
                return Ok(reply);
            }
            reply.insert("unique_new", true);
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"unique": true, "prepareUnique": false},
                )
                .map_err(command_error)?;
        }
    }
    let mut opts = collection_option_subset(doc);
    // An option given as null is an option not given.
    opts = opts
        .into_iter()
        .filter(|(_, v)| !matches!(v, Bson::Null))
        .collect();
    // The `timeseries` field of a `collMod` is a change, not a definition:
    // what is stored is the definition it produces.
    opts.remove("timeseries");
    opts.extend(ts_changes);
    // What the command said, for the change event; the defaults written out
    // below are not part of it (mongod 8.2.11).
    let given = opts.clone();
    // A `collMod` that touches validation leaves the collection with BOTH
    // `validationLevel` and `validationAction` written out (the defaults
    // where neither was ever given). An empty validator removes the
    // validator while keeping them; it is stored empty, which validates
    // nothing, and `listCollections` leaves it out. Measured on 8.2.11.
    if ["validator", "validationLevel", "validationAction"]
        .iter()
        .any(|f| opts.contains_key(*f))
    {
        let current = storage
            .get_collection_options(&ctx.db_name, &coll)
            .unwrap_or_default();
        for (field, default) in [("validationLevel", "strict"), ("validationAction", "error")] {
            if !opts.contains_key(field) {
                let kept = current.get_str(field).unwrap_or(default).to_string();
                opts.insert(field, kept);
            }
        }
    }
    // Each of `viewOn` and `pipeline` replaces its own half of the view's
    // definition and leaves the other as it was.
    if is_view {
        if let Some(view_on) = new_view_on {
            opts.insert("viewOn", view_on);
        }
        if let Some(pipeline) = new_pipeline {
            opts.insert("viewPipeline", pipeline);
        }
    }
    // `cappedSize` / `cappedMax` re-bound a capped collection. Nothing is
    // evicted here; the next insert brings the collection within the new
    // bounds, as on mongod. They were accepted and ignored until 2026-10-09.
    if capped_size_arg.is_some() || capped_max_arg.is_some() {
        if !storage
            .collection_is_capped(&ctx.db_name, &coll)
            .map_err(command_error)?
        {
            return Ok(
                CommandError::new(72, "InvalidOptions", "Collection must be capped.").into_reply(),
            );
        }
        if let Some(size) = capped_size_arg {
            opts.insert("size", int_bson(size));
        }
        if let Some(max) = capped_max_arg {
            opts.insert("max", int_bson(max));
        }
    }
    // `coll_mod` (not `set_collection_options`) so a `showExpandedEvents` change
    // stream sees the resulting `modify` event.
    storage
        .coll_mod_recording(&ctx.db_name, &coll, &opts, &given)
        .map_err(command_error)?;
    Ok(reply)
}

/// Whether two index key patterns are equal (same fields, same order, same
/// `±1` direction regardless of the numeric BSON type they're encoded as).
fn key_patterns_eq(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ak, av), (bk, bv))| ak == bk && as_i64(av) == as_i64(bv))
}

/// What mongod tells a cursor whose collection was dropped under it.
pub(crate) fn dropped_reason(storage: &dyn crate::Storage, db: &str, coll: &str) -> String {
    format!(
        "collection dropped. UUID {}",
        collection_uuid_text(storage, db, coll)
    )
}

fn collection_uuid_text(storage: &dyn crate::Storage, db: &str, coll: &str) -> String {
    // Asking for the id of a collection that is not there would register it.
    if !storage.collection_exists(db, coll).unwrap_or(false) {
        return String::new();
    }
    storage
        .collection_uuid(db, coll)
        .ok()
        .and_then(|b| uuid_text(&b))
        .unwrap_or_default()
}

/// `drop` — drop a collection.
pub fn drop(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    if let Some(field) = argtypes::unknown_field(
        doc,
        &["drop", "collectionUUID", "dropSystemBucketsCollection"],
    ) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'drop.{field}' is an unknown field."),
        ));
    }
    let coll = coll_arg(doc, "drop")?;
    let ns = format!("{}.{}", ctx.db_name, coll);
    // mongod keeps the profiler's collection while it writes to it.
    if coll == crate::profiler::PROFILE_COLLECTION
        && ctx
            .profiler
            .as_ref()
            .is_some_and(|p| p.settings(&ctx.db_name).level != 0)
    {
        return Err(CommandError::new(
            20,
            "IllegalOperation",
            "turn off profiling before dropping system.profile collection",
        ));
    }
    let storage = ctx.storage()?;
    // `system.views` holds every view's definition: dropping it drops them
    // all. It used to answer ok and leave the views in place.
    if coll == "system.views" {
        let views: Vec<String> = storage
            .list_collections(&ctx.db_name)
            .map_err(command_error)?
            .into_iter()
            .filter(|c| crate::views::is_view(storage, &ctx.db_name, c))
            .collect();
        if views.is_empty() {
            return Ok(doc! { "ok": 1.0 });
        }
        for view in &views {
            storage
                .drop_collection(&ctx.db_name, view)
                .map_err(command_error)?;
        }
        return Ok(doc! { "nIndexesWas": 1_i32, "ns": ns, "ok": 1.0 });
    }
    // Kill the collection's cursors BEFORE the storage drop, not after: the
    // drop emits an oplog entry that wakes any awaitData getMore parked on a
    // tailable cursor, and that getMore must observe the tombstone set here so
    // it reports "collection dropped" instead of re-polling a collection that
    // is already gone. Non-tailable cursors are removed outright, so a later
    // getMore is CursorNotFound (mongo-c-driver's error_document/getmore).
    // Mirrors commands.py::_drop.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&ns, &dropped_reason(storage, &ctx.db_name, &coll));
    }
    // mongod reports the index count the collection HAD, `_id_` included -- 3
    // for a collection with two secondary indexes, where this always said 1
    // (measured 8.2.11, 2026-09-30). A collection that does not exist lists
    // nothing, and that branch reports no count at all.
    // `system.profile` alone has no `_id` index to count.
    let n_indexes = match storage.list_indexes(&ctx.db_name, &coll) {
        Ok(ix) if coll == crate::profiler::PROFILE_COLLECTION => ix.len(),
        Ok(ix) => ix.len().max(1),
        Err(_) => 1,
    };
    // A time-series collection has no `_id` index to count.
    let n_indexes = if storage
        .get_collection_options(&ctx.db_name, &coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        n_indexes - 1
    } else {
        n_indexes
    };
    let was_view = crate::views::is_view(storage, &ctx.db_name, &coll);
    let existed = storage
        .drop_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;
    if !existed {
        // Modern mongod treats `drop` of a non-existent collection as an
        // idempotent success (`{ok: 1}`), not a NamespaceNotFound error. The
        // ok:1 shape also lets dispatch attach a `writeConcernError` for an
        // unsatisfiable write concern — pymongo's test_drop_collection drops an
        // already-absent collection with w:50 and asserts a WriteConcernError.
        // Mirrors commands.py::_drop.
        return Ok(doc! { "ok": 1.0 });
    }
    // A view had no indexes to count: mongod answers `{ns, ok}`.
    if was_view {
        return Ok(doc! { "ns": ns, "ok": 1.0 });
    }
    // mongod's field order: the count first, then the namespace.
    Ok(doc! { "nIndexesWas": n_indexes as i32, "ns": ns, "ok": 1.0 })
}

/// `secantusAdmin.backupArchive` — force a checkpoint and tar the WiredTiger home
/// into `outputPath` (a server-side path) for point-in-time recovery. The on-disk
/// and oplog formats match the Python server, so either server's restore tooling
/// reads the result. Mirrors the Python `secantusAdmin.backupArchive` command.
pub fn backup_archive(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let output_path = doc.get_str("outputPath").unwrap_or("");
    if output_path.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.backupArchive requires outputPath: <string>",
        ));
    }
    let storage = ctx.storage()?;
    let (path, size_bytes) = storage.create_archive(output_path).map_err(command_error)?;
    Ok(doc! { "path": path, "sizeBytes": size_bytes as i64, "ok": 1.0 })
}

/// `secantusAdmin.pruneOplog` — drop oplog rows past the retention window now,
/// returning `{pruned, ok}`. An operator-driven immediate sweep (the storage
/// engine also prunes opportunistically on every emit). Mirrors the Python
/// `secantusAdmin.pruneOplog` command.
pub fn prune_oplog(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let pruned = storage.prune_oplog().map_err(command_error)?;
    Ok(doc! { "pruned": pruned as i64, "ok": 1.0 })
}

/// `secantusAdmin.pruneTtl` — run TTL pruning across every collection now,
/// returning `{pruned, ok}` (the docs deleted). Lets callers force a
/// deterministic pass instead of waiting for the background cadence. Mirrors
/// the Python `secantusAdmin.pruneTtl` command.
pub fn prune_ttl(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let pruned = storage.prune_ttl_all().map_err(command_error)?;
    Ok(doc! { "pruned": pruned as i64, "ok": 1.0 })
}

/// `secantusAdmin.restoreArchive` — extract a backup archive (from
/// `backupArchive`) into `targetDir`, a fresh directory the operator then points
/// a *new* server at (the running server's storage is untouched — same
/// side-channel model as the Python command and real mongod's "stop, swap
/// dbpath, start"). Required: `archivePath`, `targetDir`. Optional
/// `allowExisting` (bool, default false) overlays into a non-empty target.
/// Returns `{targetDir, fileCount, archive, ok}`. Mirrors the Python
/// `secantusAdmin.restoreArchive` command.
pub fn restore_archive(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let archive_path = doc.get_str("archivePath").unwrap_or("");
    if archive_path.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.restoreArchive requires archivePath: <string>",
        ));
    }
    let target_dir = doc.get_str("targetDir").unwrap_or("");
    if target_dir.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.restoreArchive requires targetDir: <string>",
        ));
    }
    let allow_existing = doc.get_bool("allowExisting").unwrap_or(false);
    let storage = ctx.storage()?;
    let (abs_target, abs_archive, file_count) = storage
        .restore_archive(archive_path, target_dir, allow_existing)
        // A failed restore (missing/invalid archive, non-empty target) is a
        // caller error, not an internal fault — mirror the Python handler's
        // IllegalOperation(20) rather than InternalError.
        .map_err(|e| CommandError::new(20, "IllegalOperation", command_error(e).errmsg))?;
    Ok(doc! {
        "targetDir": abs_target,
        "fileCount": file_count as i64,
        "archive": abs_archive,
        "ok": 1.0,
    })
}

/// `secantusAdmin.archiveBaseSnapshot` — take a PITR v2 base snapshot into
/// `archiveDir` (`base-<head>.tar.gz`). Pair with a server started with
/// `--oplog-archive-dir <archiveDir>` so pruned oplog rows are archived as
/// segments there too; recovery then stitches the newest base ≤ T plus the
/// segments. Mirrors the Python `secantusAdmin.archiveBaseSnapshot` command.
pub fn archive_base_snapshot(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let archive_dir = doc.get_str("archiveDir").unwrap_or("");
    if archive_dir.is_empty() {
        return Err(CommandError::new(
            14,
            "TypeMismatch",
            "secantusAdmin.archiveBaseSnapshot requires archiveDir: <string>",
        ));
    }
    let storage = ctx.storage()?;
    let (path, size_bytes) = storage
        .archive_base_snapshot(archive_dir)
        .map_err(command_error)?;
    Ok(doc! { "path": path, "sizeBytes": size_bytes as i64, "ok": 1.0 })
}

/// `listCollections` — a cursor over the collections in the database, honouring
/// `filter` (a query predicate over each entry) and `nameOnly`. Each entry's
/// `options` reflects the collection's stored options (capped / validator /
/// collation / timeseries / …) so drivers introspecting them see the real
/// values. Mirrors `commands._list_collections`.
pub fn list_collections(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    argtypes::require_object(doc, "filter", "listCollections.filter")?;
    argtypes::require_object(doc, "cursor", "listCollections.cursor")?;
    argtypes::require_cursor_options(doc, "listCollections.cursor")?;
    argtypes::require_bool_field(doc, "nameOnly", "listCollections.nameOnly")?;
    match doc.get("authorizedCollections") {
        None | Some(Bson::Null | Bson::Boolean(_)) => {}
        Some(other) => {
            return Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'listCollections.authorizedCollections' is the wrong type \
                     '{}', expected type 'bool'",
                    secantus_core::query::bson_type_name(other)
                ),
            ))
        }
    }
    // mongod parses the filter before it lists anything, so an operator it
    // does not know is refused even when nothing would have been tested.
    if let Some(Bson::Document(f)) = doc.get("filter") {
        if let Some((code, msg)) = argtypes::unknown_operator_in_filter(f) {
            return Err(CommandError::new(
                code,
                crate::util::error_code_name(code),
                msg,
            ));
        }
    }
    const FIELDS: &[&str] = &[
        "listCollections",
        "cursor",
        "filter",
        "nameOnly",
        "authorizedCollections",
        "includePendingDrops",
    ];
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'listCollections.{key}' is an unknown field."),
        ));
    }
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    let name_only = bool_field(doc, "nameOnly", false);
    let mut names = listed_collection_names(storage, &ctx.db_name, ctx.replica_set_name.is_some())?;
    // `authorizedCollections` lists only the collections the user has any
    // action on, which leaves out a system collection unless the user has
    // `anyResource` or that collection's name (measured 8.2.11: `read` on
    // the database lists `c1` and `c2` and not `system.views`).
    if bool_field(doc, "authorizedCollections", false) {
        if let Some(az) = crate::rbac::enforced(ctx)? {
            let db = ctx.db_name.clone();
            names.retain(|n| {
                az.any_action_on(&crate::privileges::Resource::Ns {
                    db: db.clone(),
                    collection: n.clone(),
                })
            });
        }
    }
    let mut entries = collection_entries(storage, &ctx.db_name, &names)?;
    // With `nameOnly` the filter sees what is returned, the name and the
    // type: `{"options.capped": true}` then matches nothing (mongod 8.2.11).
    if name_only {
        for e in &mut entries {
            let name = e.get_str("name").unwrap_or("").to_string();
            let ty = e.get_str("type").unwrap_or("collection").to_string();
            *e = doc! { "name": name, "type": ty };
        }
    }
    if let Some(f) = filter {
        entries.retain(|e| {
            secantus_core::query::matches(e, f, &Document::new(), None).unwrap_or(false)
        });
    }
    let ns = format!("{}.$cmd.listCollections", ctx.db_name);
    // Honour `cursor: {batchSize: N}` so a client that asks for a small batch
    // gets a real getMore (drivers' "listCollections getMore is monitored"
    // tests force this); absent ⇒ the wire default.
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    let (first, cid) = split_into_cursor(encode_docs(entries)?, batch_size, &ns, cursors, true)?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// `listCollections`' entry for each of `names` in `db`, followed by those
/// of what mongod keeps beside them and this server derives: a
/// `system.buckets.<name>` per time-series collection, and `system.views`.
pub(crate) fn collection_entries(
    storage: &dyn crate::storage::Storage,
    db: &str,
    names: &[String],
) -> Result<Vec<Document>, CommandError> {
    let mut entries: Vec<Document> = Vec::with_capacity(names.len());
    for n in names {
        let mut options = storage
            .get_collection_options(db, n)
            .map_err(command_error)?;
        // A `viewOn` collection is a read-only view: type "view", readOnly true,
        // no `_id` index, and the stored `viewPipeline` surfaces as `pipeline`.
        let is_view = options.contains_key("viewOn");
        if let Some(p) = options.remove("viewPipeline") {
            options.insert("pipeline", p);
        }
        // An empty validator is how a removed one is stored.
        if matches!(options.get("validator"), Some(Bson::Document(v)) if v.is_empty()) {
            options.remove("validator");
        }
        // `uuid` is an internal option (the collection identity) — it's surfaced
        // under `info.uuid`, not as a collection option. Strip it from `options`.
        options.remove("uuid");
        // Options at their defaults are not listed: pre-images switched off,
        // and the simple collation.
        // mongod lists the options in its own order, not the order given.
        const ORDER: [&str; 10] = [
            "capped",
            "size",
            "max",
            "storageEngine",
            "validator",
            "validationLevel",
            "validationAction",
            "indexOptionDefaults",
            "viewOn",
            "pipeline",
        ];
        let mut ordered = Document::new();
        for key in ORDER {
            if let Some(v) = options.remove(key) {
                ordered.insert(key, v);
            }
        }
        ordered.extend(std::mem::take(&mut options));
        options = ordered;
        options = options
            .into_iter()
            .filter(|(_, v)| !matches!(v, Bson::Null))
            .collect();
        let off = |o: &Document, field: &str, key: &str, value: Bson| {
            o.get_document(field)
                .is_ok_and(|d| d.get(key) == Some(&value))
        };
        if off(
            &options,
            "changeStreamPreAndPostImages",
            "enabled",
            Bson::Boolean(false),
        ) {
            options.remove("changeStreamPreAndPostImages");
        }
        if off(
            &options,
            "collation",
            "locale",
            Bson::String("simple".into()),
        ) {
            options.remove("collation");
        }
        // A collection-level TTL turned `off` by `collMod` is stored as null.
        if matches!(options.get("expireAfterSeconds"), Some(Bson::Null)) {
            options.remove("expireAfterSeconds");
        }
        // mongod stores/reports capped `size` / `max` as int32; we may hold the
        // driver-sent int64. Normalise so the round-tripped options match.
        for k in ["size", "max"] {
            if let Some(Bson::Int64(v)) = options.get(k) {
                let v = *v;
                options.insert(k, int_bson(v));
            }
        }
        let coll_type = if is_view {
            "view"
        } else if options.contains_key("timeseries") {
            "timeseries"
        } else {
            "collection"
        };
        // `info.uuid` is BinData(4) — driver CollectionSpecification readers
        // (e.g. the go driver) read it as a Binary, so it must be present and the
        // right type.
        let mut info = doc! { "readOnly": is_view };
        // A view is a definition, not a collection: it has no uuid.
        let uuid = if db == "local" && n == "oplog.rs" {
            Ok(crate::serverinfo::oplog_uuid(storage))
        } else {
            storage.collection_uuid(db, n)
        };
        if let (false, Ok(uuid)) = (is_view, uuid) {
            if uuid.len() == 16 {
                info.insert(
                    "uuid",
                    Bson::Binary(bson::Binary {
                        subtype: bson::spec::BinarySubtype::Uuid,
                        bytes: uuid,
                    }),
                );
            }
        }
        // Captured before `options` is moved into the entry below.
        // An oplog has no `_id` index either (`autoIndexId: false`).
        let is_clustered = options.contains_key("clusteredIndex")
            || options.get("autoIndexId") == Some(&Bson::Boolean(false));
        let mut entry = doc! {
            "name": n,
            "type": coll_type,
            "options": Bson::Document(options),
            "info": info,
        };
        // A clustered collection has no separate `_id_` index (the clustering
        // key IS the index), so mongod omits `idIndex` for it — same as views.
        // Nor has `system.profile` (mongod 8.2.11).
        if !is_view
            && !is_clustered
            && coll_type != "timeseries"
            && n.as_str() != crate::profiler::PROFILE_COLLECTION
        {
            let mut id_index = doc! {
                "v": 2,
                "key": { "_id": 1 },
                "name": "_id_",
            };
            let collation = entry
                .get_document("options")
                .ok()
                .and_then(|o| o.get_document("collation").ok());
            if let Some(collation) = collation {
                id_index.insert("collation", collation.clone());
            }
            entry.insert("idIndex", id_index);
        }
        entries.push(entry);
    }
    // What mongod keeps views and time-series collections in: `system.views`,
    // and a `system.buckets.<name>` per time-series collection. Neither is
    // stored here, and both read as mongod's do (see `ts_buckets`).
    let mut derived: Vec<Document> = Vec::new();
    for entry in &entries {
        let (Ok(name), Ok("timeseries")) = (entry.get_str("name"), entry.get_str("type")) else {
            continue;
        };
        let Ok(definition) = entry
            .get_document("options")
            .and_then(|o| o.get_document("timeseries"))
        else {
            continue;
        };
        let time = definition.get_str("timeField").unwrap_or_default();
        let mut info = doc! { "readOnly": false };
        if let Ok(uuid) = storage.collection_uuid(db, name) {
            info.insert("uuid", crate::ts_buckets::derived_uuid(&uuid, 1));
        }
        derived.push(doc! {
            "name": format!("{}{name}", crate::ts_buckets::BUCKETS_PREFIX),
            "type": "collection",
            "options": {
                "validator": crate::ts_buckets::validator(time),
                "clusteredIndex": true,
                "timeseries": definition.clone(),
            },
            "info": info,
        });
    }
    if entries
        .iter()
        .any(|e| matches!(e.get_str("type"), Ok("view" | "timeseries")))
    {
        derived.push(doc! {
            "name": "system.views",
            "type": "collection",
            "options": {},
            "info": {
                "readOnly": false,
                "uuid": crate::ts_buckets::derived_uuid(db.as_bytes(), 2),
            },
            "idIndex": { "v": 2, "key": { "_id": 1 }, "name": "_id_" },
        });
    }
    // Each bucket namespace follows its collection, and `system.views` is
    // last. mongod's own order is not one a client can rely on.
    for entry in derived {
        let after = entry
            .get_str("name")
            .ok()
            .and_then(|n| n.strip_prefix(crate::ts_buckets::BUCKETS_PREFIX))
            .and_then(|base| entries.iter().position(|e| e.get_str("name") == Ok(base)));
        match after {
            Some(i) => entries.insert(i + 1, entry),
            None => entries.push(entry),
        }
    }
    Ok(entries)
}

/// The collections `listCollections` names in `db`. `set_member`: this
/// server is a replica-set member, whose oplog is `local.oplog.rs`.
pub(crate) fn listed_collection_names(
    storage: &dyn crate::storage::Storage,
    db: &str,
    set_member: bool,
) -> Result<Vec<String>, CommandError> {
    let mut names = storage.list_collections(db).map_err(command_error)?;
    // A set member's oplog is `local.oplog.rs`. Here it is the oplog table,
    // with no catalog entry, so it is added by name.
    if db == "local" && set_member && !names.iter().any(|n| n == "oplog.rs") {
        names.push("oplog.rs".to_string());
        names.sort();
    }
    Ok(names)
}

/// `listIndexes` — a cursor over the indexes of a collection.
pub fn list_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // Nullable here, unlike aggregate's -- mongod accepts `cursor: null`.
    argtypes::require_object(doc, "cursor", "listIndexes.cursor")?;
    argtypes::require_cursor_options(doc, "listIndexes.cursor")?;
    const FIELDS: &[&str] = &[
        "listIndexes",
        "cursor",
        "includeBuildUUIDs",
        "includeIndexBuildInfo",
        "isTimeseriesNamespace",
        "rawData",
    ];
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'listIndexes.{key}' is an unknown field."),
        ));
    }
    let coll = coll_arg(doc, "listIndexes")?;
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let oplog = ctx.db_name == "local" && coll == "oplog.rs" && ctx.replica_set_name.is_some();
    let Some(indexes) = listed_indexes(storage, &ctx.db_name, &coll, oplog)? else {
        // A collection that exists always has at least the synthesised `_id_`
        // index, so none at all means the namespace doesn't exist — mongod
        // errors NamespaceNotFound (mongo-ruby-driver `Index::View#each ...
        // collection does not exist`). Mirrors commands.py's `if not indexes`.
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns does not exist: {}.{}", ctx.db_name, coll),
        )
        .into_reply());
    };
    // mongod reports a listIndexes cursor under the PLAIN collection namespace
    // (`db.coll`), not a `$cmd.` pseudo-namespace -- probed on 8.3.4. That is
    // also what drivers put in the follow-up getMore's `collection` field, so a
    // `$cmd.listIndexes.<coll>` namespace failed the getMore ownership check and
    // made the second batch unreachable (CursorNotFound), i.e. listIndexes could
    // not be paginated at all. Contrast `listCollections`, which really is
    // `db.$cmd.listCollections` on mongod, and the collectionless `aggregate: 1`
    // form, which really is `db.$cmd.aggregate` -- both already correct.
    let ns = format!("{}.{}", ctx.db_name, coll);
    // Honour `cursor: {batchSize: N}` so a client asking for a small batch gets a
    // real getMore round-trip (the Go driver's
    // `TestIndexView/list/getMore_commands_are_monitored` asserts a getMore fires
    // at batchSize 2); absent ⇒ the wire default. Mirrors `list_collections`.
    let batch_size = doc
        .get("cursor")
        .and_then(Bson::as_document)
        .and_then(|c| c.get("batchSize"))
        .and_then(as_i64)
        .unwrap_or(DEFAULT_BATCH_SIZE as i64);
    // A negative batchSize is rejected, not clamped. mongo-ruby-driver's
    // `Collection#indexes when a session is provided` uses `batch_size: -100`
    // as its deliberately-failing operation and asserts an OperationFailure.
    // Mirrors `commands._list_indexes`.
    if batch_size < 0 {
        // 6.0 answered 51024 Location51024 here; 8.x answers 2 BadValue with
        // the same message. Mirrors `commands._require_non_negative_number`.
        return Ok(CommandError::new(
            2,
            "BadValue",
            format!("BSON field 'batchSize' value must be >= 0, actual value {batch_size}"),
        )
        .into_reply());
    }
    let (first, cid) = split_into_cursor(encode_docs(indexes)?, batch_size, &ns, cursors, true)?;
    Ok(doc! {
        "cursor": { "id": Bson::Int64(cid), "ns": ns, "firstBatch": docs_to_bson(first)? },
        "ok": 1.0,
    })
}

/// The indexes of `db.coll` as `listIndexes` lists them, `None` when the
/// collection does not exist. `oplog`: the collection is a set member's
/// oplog, which exists and has no index at all.
pub(crate) fn listed_indexes(
    storage: &dyn crate::storage::Storage,
    db: &str,
    coll: &str,
    oplog: bool,
) -> Result<Option<Vec<Document>>, CommandError> {
    let mut indexes = storage.list_indexes(db, coll).map_err(command_error)?;
    // `system.profile` exists and has no index at all (mongod creates it
    // without one), like a set member's oplog.
    let unindexed = coll == crate::profiler::PROFILE_COLLECTION
        && storage.collection_exists(db, coll).unwrap_or(false);
    if indexes.is_empty() && !oplog && !unindexed {
        return Ok(None);
    }
    // `_id_` is always listed first, as on mongod. The rest come back in name
    // order here (mongod: creation order), which put an index named `$**_1`
    // or `A_1` ahead of it.
    indexes.sort_by_key(|ix| ix.get_str("name") != Ok("_id_"));
    // `hidden` is listed only on an index that is hidden.
    for ix in &mut indexes {
        if ix.get_bool("hidden") == Ok(false) {
            ix.remove("hidden");
        }
        // Listed only while it is armed: a unique index has nothing to prepare.
        if ix.get_bool("prepareUnique") == Ok(false) || ix.get_bool("unique") == Ok(true) {
            ix.remove("prepareUnique");
        }
    }
    // A time-series collection has no `_id` index to list.
    if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        indexes.retain(|ix| ix.get_str("name") != Ok("_id_"));
    }
    // `multikey` is SecantusDB's internal catalog flag. mongod keeps the
    // equivalent in the durable catalog and never echoes it from `listIndexes`
    // (probed 6.0.16) — drivers see it only as explain's `isMultiKey`. Keep it
    // off the wire. Mirrors commands.py.
    // `entryFormat` is likewise internal — the on-disk index-entry layout
    // version (see `ENTRY_FORMAT_RECORDID`). mongod has no such field.
    for ix in &mut indexes {
        ix.remove("multikey");
        ix.remove("entryFormat");
        ix.remove("collationKeys");
        ix.remove("geoKeys");
        ix.remove("pythonKeys");
    }
    // The `_id` index of a collection with a default collation carries it.
    if let Some(default) = crate::util::default_collation(storage, db, coll) {
        for ix in &mut indexes {
            if ix.get_str("name") == Ok("_id_") {
                ix.insert("collation", default.spec().clone());
            }
        }
    }
    // A clustered collection's clustering key IS its index: mongod reports a
    // single entry carrying `clustered: true` (with the user's name) in place of
    // the synthesised `_id_`. Mirrors commands.py.
    let options = storage.get_collection_options(db, coll).unwrap_or_default();
    if let Ok(ci) = options.get_document("clusteredIndex") {
        let mut clustered = doc! {
            "v": ci.get_i32("v").unwrap_or(2),
            "key": { "_id": 1i32 },
            "name": ci.get_str("name").unwrap_or("_id_").to_string(),
            "unique": true,
        };
        // The collection's own expiry is its clustered index's (measured
        // 8.2.11, 2026-10-13); it was left out.
        if let Some(seconds) = options.get("expireAfterSeconds") {
            // As an int where it fits one, which is how mongod lists it.
            let listed = match crate::util::as_i64(seconds).map(i32::try_from) {
                Some(Ok(n)) => Bson::Int32(n),
                _ => seconds.clone(),
            };
            clustered.insert("expireAfterSeconds", listed);
        }
        clustered.insert("clustered", true);
        let mut rest: Vec<Document> = indexes
            .into_iter()
            .filter(|ix| ix.get_str("name") != Ok("_id_"))
            .collect();
        indexes = std::iter::once(clustered).chain(rest.drain(..)).collect();
    }
    Ok(Some(indexes))
}

/// `createIndexes` — create one or more indexes (auto-creating the collection).
/// Field-level query operators a `partialFilterExpression` may use. A `$`-key in
/// a field clause outside this set is an unknown operator (rejected).
const KNOWN_FIELD_OPS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$regex",
    "$options",
    "$mod",
    "$size",
    "$all",
    "$elemMatch",
    "$not",
    "$bitsAllSet",
    "$bitsAnySet",
    "$bitsAllClear",
    "$bitsAnyClear",
    "$geoWithin",
    "$within",
    "$geoIntersects",
    "$near",
    "$nearSphere",
    "$comment",
    "$maxDistance",
    "$minDistance",
    "$geometry",
    "$center",
    "$centerSphere",
    "$box",
    "$polygon",
];

/// Document-level query operators accepted in a `partialFilterExpression` (the
/// logical / expression operators; `$and`/`$or`/`$nor` are recursed below).
const KNOWN_DOC_OPS: &[&str] = &["$expr", "$comment", "$text", "$where", "$jsonSchema"];

/// A conservative filter-validity check for `partialFilterExpression`:
/// `Some(reason)` for a clearly-invalid construct (a malformed `$and`/`$or`/`$nor`
/// or an unknown operator), else `None`. Deliberately lenient — it only rejects
/// operators outside the known sets, so a valid filter is never wrongly refused.
fn invalid_partial_filter(filter: &Document) -> Option<String> {
    for (k, v) in filter {
        if let Some(stripped) = k.strip_prefix('$') {
            match k.as_str() {
                "$and" | "$or" | "$nor" => match v {
                    Bson::Array(arr) => {
                        for elem in arr {
                            match elem {
                                Bson::Document(sub) => {
                                    if let Some(r) = invalid_partial_filter(sub) {
                                        return Some(r);
                                    }
                                }
                                _ => return Some(format!("{k} elements must be documents")),
                            }
                        }
                    }
                    _ => return Some(format!("{k} must be an array")),
                },
                _ if KNOWN_DOC_OPS.contains(&k.as_str()) => {}
                _ => return Some(format!("unknown top-level operator ${stripped}")),
            }
        } else if let Bson::Document(opd) = v {
            // A field clause whose value is an operator document: every `$`-key
            // must be a known field operator.
            if let Some(bad) = opd
                .keys()
                .find(|kk| kk.starts_with('$') && !KNOWN_FIELD_OPS.contains(&kk.as_str()))
            {
                return Some(format!("unknown operator {bad}"));
            }
        }
    }
    None
}

/// Whether a BSON value is "falsy" the way mongod treats default index options
/// (`hidden` / `sparse` / `unique`): `false`, `0`, `0.0`, or null.
fn is_falsy(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => !b,
        Bson::Int32(i) => *i == 0,
        Bson::Int64(i) => *i == 0,
        Bson::Double(d) => *d == 0.0,
        Bson::Null => true,
        _ => false,
    }
}

pub fn create_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // `indexes` is an array of specs; a scalar there used to report ok:1 and
    // create NOTHING, so a driver believed an index existed that did not.
    // An explicit `indexes: null` means ABSENT on 8.x (40414), identical to
    // omitting it; the fields INSIDE a spec quote the whole spec back. Neither
    // follows the generic families -- probed per slot.
    argtypes::require_index_specs(doc)?;
    if let Some(bson::Bson::Array(specs)) = doc.get("indexes") {
        for spec in specs {
            if let bson::Bson::Document(spec) = spec {
                argtypes::require_index_spec_field(spec, "key", "an object", |v| {
                    matches!(v, bson::Bson::Document(_))
                })?;
                argtypes::require_index_spec_field(spec, "name", "a string", |v| {
                    matches!(v, bson::Bson::String(_))
                })?;
                for field in ["collation", "partialFilterExpression"] {
                    argtypes::require_index_spec_field(spec, field, "an object", |v| {
                        matches!(v, bson::Bson::Document(_))
                    })?;
                }
                // Two more families INSIDE one spec: the bool options quote the
                // spec back with mongod's unclosed quote, and the TTL option
                // answers 67 rather than 14.
                for field in ["unique", "sparse"] {
                    argtypes::require_index_spec_bool(spec, field)?;
                }
                argtypes::require_index_spec_ttl(spec, "expireAfterSeconds")?;
            }
        }
    }
    let coll = coll_arg(doc, "createIndexes")?;
    if let Some(e) = crate::admin::invalid_write_namespace(&ctx.db_name, &coll) {
        return Ok(e.into_reply());
    }
    let storage = ctx.storage()?;
    let specs: Vec<Bson> = match doc.get("indexes") {
        Some(Bson::Array(a)) => a.clone(),
        _ => Vec::new(),
    };

    // `commitQuorum` (4.4+) accepts an integer, `"majority"`, or `"votingMembers"`;
    // any other string is an unknown write-concern mode (mongo-ruby-driver's
    // `commit_quorum value is not supported` pins this). Mirrors commands.py.
    if let Some(cq) = doc.get("commitQuorum") {
        let ok = matches!(cq, Bson::Int32(_) | Bson::Int64(_))
            || matches!(cq.as_str(), Some("majority") | Some("votingMembers"));
        if !ok {
            let shown = match cq {
                Bson::String(s) => format!("'{s}'"),
                other => format!("{other}"),
            };
            return Ok(CommandError::new(
                79,
                "UnknownReplWriteConcern",
                format!("No write concern mode named {shown} found in replica set configuration"),
            )
            .into_reply());
        }
    }

    // A number is a boolean to an index option: `unique: 1` is a unique
    // index. It used to be read as "not unique".
    let specs: Vec<Bson> = specs
        .into_iter()
        .map(|spec| match spec {
            Bson::Document(mut s) => {
                for flag in ["unique", "sparse", "hidden", "background"] {
                    if let Some(n) = s.get(flag).and_then(|v| match v {
                        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => as_i64(v),
                        _ => None,
                    }) {
                        s.insert(flag, n != 0);
                    }
                }
                // mongod stores a TTL as an int32 whatever number type it was
                // given, truncating a fraction. NaN is left for the check
                // below to refuse.
                let seconds = s.get("expireAfterSeconds").and_then(|v| match v {
                    Bson::Double(d) if d.is_nan() => None,
                    Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_) => as_i64(v),
                    _ => None,
                });
                if let Some(n) = seconds.filter(|n| (0..=i64::from(i32::MAX)).contains(n)) {
                    s.insert("expireAfterSeconds", n as i32);
                }
                Bson::Document(s)
            }
            other => other,
        })
        .collect();
    // Every spec is checked before anything is created: an invalid spec used
    // to leave the collection, and the specs before it, created.
    if specs.is_empty() {
        return Ok(
            CommandError::new(2, "BadValue", "Must specify at least one index to create")
                .into_reply(),
        );
    }
    for spec in &specs {
        if let Bson::Document(s) = spec {
            if let Some(problem) = index_spec_problem(s) {
                return Ok(problem.into_reply());
            }
        }
    }

    let before = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    // createIndexes implicitly creates the collection if absent.
    let created_coll = storage
        .create_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;

    let existing = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    // A time-series collection lists no `_id` index and restricts the rest.
    let ts_fields = storage
        .get_collection_options(&ctx.db_name, &coll)
        .ok()
        .and_then(|o| crate::timeseries::Fields::of(&o));
    let unlisted = usize::from(ts_fields.is_some());
    let before = before.saturating_sub(unlisted);
    let mut any_created = false;
    let mut any_existed = false;
    for spec in &specs {
        let Bson::Document(s) = spec else { continue };
        if let Some(e) = ts_fields
            .as_ref()
            .and_then(|f| crate::timeseries::index_problem(s, f))
        {
            return Ok(e.into_reply());
        }
        let key = s
            .get("key")
            .and_then(Bson::as_document)
            .cloned()
            .unwrap_or_default();
        // mongod refuses these two before anything else about the spec, and
        // quotes the spec as given (measured 8.2.11, 2026-10-01). Both used to
        // be ACCEPTED: an empty key built an index over nothing, and a missing
        // name was invented from the key.
        let spec_text = argtypes::render_stage_value(&Bson::Document(s.clone()));
        // The `_id` index always exists, under its own name: asking for it
        // again, whatever the request calls it, creates nothing.
        if key.len() == 1 && key.get("_id").and_then(as_i64) == Some(1) {
            // It has the collection's collation and takes no other.
            let full = |c: Option<secantus_core::collation::Collation>| match c {
                Some(c) => c.spec().clone(),
                None => doc! { "locale": "simple" },
            };
            let own = full(crate::util::default_collation(storage, &ctx.db_name, &coll));
            let asked = match s.get_document("collation") {
                Ok(given) if !given.is_empty() => full(
                    secantus_core::collation::parse_strict(
                        given,
                        secantus_core::collation::Context::COMMAND,
                    )
                    .map_err(crate::util::collation_error)?,
                ),
                _ => own.clone(),
            };
            if asked != own {
                // Nothing is built, the collection included.
                if created_coll {
                    storage
                        .drop_collection(&ctx.db_name, &coll)
                        .map_err(command_error)?;
                }
                return Ok(CommandError::new(
                    2,
                    "BadValue",
                    format!(
                        "The _id index must have the same collation as the collection. Index \
                         collation: {}, collection collation: {}",
                        argtypes::render_stage_value(&Bson::Document(asked)),
                        argtypes::render_stage_value(&Bson::Document(own)),
                    ),
                )
                .into_reply());
            }
            any_existed = true;
            continue;
        }
        if key.is_empty() {
            return Ok(CommandError::new(
                67,
                "CannotCreateIndex",
                format!(
                    "Error in specification {spec_text} :: caused by :: Index keys cannot be empty."
                ),
            )
            .into_reply());
        }
        // A string key value names an index PLUGIN. mongod knows a handful; any
        // other name is refused as unknown, quoting the spec (measured 8.2.11,
        // 2026-10-01). Known-but-unsupported types keep the storage layer's own
        // refusal.
        if let Some(plugin) = key.values().find_map(|v| match v {
            Bson::String(p) if !KNOWN_INDEX_PLUGINS.contains(&p.as_str()) => Some(p.clone()),
            _ => None,
        }) {
            let why = match plugin.as_str() {
                "" => "Values in the index key pattern cannot be empty strings".to_string(),
                "geoHaystack" => {
                    "GeoHaystack indexes cannot be created in version 5.0 and above".to_string()
                }
                other => format!("Unknown index plugin '{other}'"),
            };
            return Ok(CommandError::new(
                67,
                "CannotCreateIndex",
                format!("Error in specification {spec_text} :: caused by :: {why}"),
            )
            .into_reply());
        }
        // A `2dsphere` index is one of three formats.
        if key
            .values()
            .any(|v| matches!(v, Bson::String(p) if p == "2dsphere"))
        {
            if let Some(version) = s.get("2dsphereIndexVersion") {
                let whole = match version {
                    Bson::Int32(n) => Some(i64::from(*n)),
                    Bson::Int64(n) => Some(*n),
                    Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
                    _ => None,
                };
                let number = matches!(
                    version,
                    Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
                );
                if !number {
                    return Ok(CommandError::new(
                        14,
                        "TypeMismatch",
                        format!(
                            "Error in specification {spec_text} :: caused by :: The field \
                             '2dsphereIndexVersion' must be a number, but got {}",
                            secantus_core::query::bson_type_name(version)
                        ),
                    )
                    .into_reply());
                }
                if let Some(n) = whole.filter(|n| !(1..=3).contains(n)) {
                    // mongod has added the index version to the spec by now.
                    let with_v = spec_text
                        .strip_suffix(" }")
                        .map_or(spec_text.clone(), |open| format!("{open}, v: 2 }}"));
                    return Ok(CommandError::new(
                        67,
                        "CannotCreateIndex",
                        format!(
                            "Error in specification {with_v} :: caused by :: unsupported geo \
                             index version {{ 2dsphereIndexVersion : 2dsphereIndexVersion: {n} }}, \
                             only versions: [1,2,3] are supported"
                        ),
                    )
                    .into_reply());
                }
            }
        }
        let Some(name) = s.get("name").and_then(Bson::as_str).map(str::to_string) else {
            return Ok(CommandError::new(
                9,
                "FailedToParse",
                format!(
                    "Error in specification {spec_text} :: caused by :: The 'name' field is a \
                     required property of an index specification"
                ),
            )
            .into_reply());
        };
        // Unknown fields on the spec itself are rejected, not ignored.
        let spec_opts: Document = s
            .iter()
            .filter(|(k, _)| k.as_str() != "key" && k.as_str() != "name")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(unknown) = first_unknown_field(&spec_opts, INDEX_SPEC_KNOWN_OPTIONS) {
            return Ok(CommandError::new(
                40415,
                "IDLUnknownField",
                format!("Error in specification {s:?}: the field '{unknown}' is an unknown field"),
            )
            .into_reply());
        }
        // Guard the index name against an embedded NUL before it reaches the
        // WT key encoder (see crate::nul_in_namespace / #139).
        if let Some(e) = crate::nul_in_namespace("index name", &name) {
            return Ok(e.into_reply());
        }
        // `partialFilterExpression` must be a document and a parseable filter —
        // mongod rejects a non-document, unknown operators (`{x: {$asdasd: 3}}`),
        // and malformed logical operators (`{$and: 5}`). (pymongo's
        // `test_index_filter` pins these.)
        if let Some(pfe) = s.get("partialFilterExpression") {
            match pfe {
                Bson::Document(f) => {
                    if let Some(reason) = invalid_partial_filter(f) {
                        return Ok(CommandError::new(
                            2,
                            "BadValue",
                            format!("Error in specification, partialFilterExpression is invalid: {reason}"),
                        )
                        .into_reply());
                    }
                }
                _ => {
                    return Ok(CommandError::new(
                        2,
                        "BadValue",
                        "partialFilterExpression must be a document",
                    )
                    .into_reply())
                }
            }
        }
        // `wildcardProjection` is only valid on a wildcard index ({$**:1} /
        // {f.$**:1}) and must be a non-empty document (mongo-ruby-driver's
        // invalid-wildcard-projection tests). Mirrors commands.py.
        if let Some(wcp) = s.get("wildcardProjection") {
            let nonempty_doc = matches!(wcp, Bson::Document(d) if !d.is_empty());
            if !nonempty_doc {
                return Ok(CommandError::new(
                    67,
                    "CannotCreateIndex",
                    format!(
                        "Error in specification {{ key: {key:?}, wildcardProjection: {wcp:?} }} \
                         :: caused by :: wildcardProjection must be a non-empty object"
                    ),
                )
                .into_reply());
            }
            let is_wildcard = key.keys().any(|k| k == "$**" || k.ends_with(".$**"));
            if !is_wildcard {
                return Ok(CommandError::new(
                    67,
                    "CannotCreateIndex",
                    format!(
                        "Error in specification {{ key: {key:?}, wildcardProjection: {wcp:?} }} \
                         :: caused by :: wildcardProjection is only allowed on wildcard indexes"
                    ),
                )
                .into_reply());
            }
        }
        // mongod stores only the non-default form of hidden / sparse / unique: a
        // falsy value is dropped so it doesn't come back from listIndexes
        // (mongo-ruby-driver `hidden is false` asserts hidden isn't echoed).
        let mut spec = s.clone();
        for opt in ["hidden", "sparse", "unique"] {
            if spec.get(opt).is_some_and(is_falsy) {
                spec.remove(opt);
            }
        }
        // The collation is stored spelled out in full, as mongod stores it:
        // the one given, else the collection's default. `simple` is stored
        // as no collation at all.
        match s.get_document("collation") {
            Ok(given) if given.is_empty() => {
                return Ok(CommandError::new(
                    2,
                    "BadValue",
                    format!(
                        "Error in specification {spec_text} :: caused by :: The field \
                         'collation' cannot be an empty object."
                    ),
                )
                .into_reply());
            }
            Ok(given) => {
                match secantus_core::collation::parse_strict(
                    given,
                    secantus_core::collation::Context::COMMAND,
                ) {
                    Ok(Some(c)) => {
                        spec.insert("collation", c.spec().clone());
                    }
                    Ok(None) => {
                        spec.remove("collation");
                    }
                    Err(e) => {
                        let mut shown = s.clone();
                        if !shown.contains_key("v") {
                            shown.insert("v", 2i32);
                        }
                        return Ok(CommandError::new(
                            e.code,
                            crate::util::error_code_name(e.code),
                            format!(
                                "failed to add collation information to index spec for index \
                                 creation: {} :: caused by :: {}",
                                argtypes::render_stage_value(&Bson::Document(shown)),
                                e.message
                            ),
                        )
                        .into_reply());
                    }
                }
            }
            Err(_) => {
                if let Some(default) = crate::util::default_collation(storage, &ctx.db_name, &coll)
                {
                    spec.insert("collation", default.spec().clone());
                }
            }
        }
        if let Some(e) = index_conflict(&existing, &name, &key, &spec) {
            return Ok(e.into_reply());
        }
        let created = match storage.create_index(&ctx.db_name, &coll, &name, &key, &spec) {
            Ok(c) => c,
            // A unique index over data that already holds duplicates: mongod
            // fails the BUILD, under its own wrapper, and still carries
            // keyPattern / keyValue (measured 8.2.11, 2026-10-01).
            Err(StorageError::DuplicateKey(info)) => {
                let coll_uuid = storage
                    .collection_uuid(&ctx.db_name, &coll)
                    .ok()
                    .and_then(|b| uuid_text(&b))
                    .unwrap_or_default();
                let build = uuid_text(&bson::uuid::Uuid::new().bytes()).unwrap_or_default();
                let mut reply = doc! {
                    "ok": 0.0,
                    "errmsg": format!(
                        "Index build failed: {build}: Collection {}.{coll} ( {coll_uuid} ) \
                         :: caused by :: {}",
                        ctx.db_name, info.errmsg
                    ),
                    "code": 11000,
                    "codeName": "DuplicateKey",
                };
                if let Some(kp) = info.key_pattern {
                    reply.insert("keyPattern", kp);
                }
                if let Some(kv) = info.key_value {
                    reply.insert("keyValue", kv);
                }
                if let Some(extra) = info.extra {
                    reply.extend(extra);
                }
                return Ok(reply);
            }
            // A document the index cannot hold stops the build's scan, and
            // mongod reports it under the same wrapper.
            Err(StorageError::WriteError { code, errmsg, .. })
                if errmsg.starts_with("collection scan stopped.") =>
            {
                let coll_uuid = storage
                    .collection_uuid(&ctx.db_name, &coll)
                    .ok()
                    .and_then(|b| uuid_text(&b))
                    .unwrap_or_default();
                let build = uuid_text(&bson::uuid::Uuid::new().bytes()).unwrap_or_default();
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    format!(
                        "Index build failed: {build}: Collection {}.{coll} ( {coll_uuid} ) :: \
                         caused by :: {errmsg}",
                        ctx.db_name
                    ),
                ));
            }
            Err(e) => return Err(command_error(e)),
        };
        any_created |= created;
        any_existed |= !created;
    }

    let after = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len()
        .saturating_sub(unlisted);
    // mongod's reply, in its order: the two counts, whether the collection
    // was created (left out when nothing was built), the commit quorum a
    // replica-set member reports, and the no-op note.
    let all_existed = !any_created && !specs.is_empty();
    // A collection created here already has its `_id` index when counted.
    let before = if created_coll && ts_fields.is_none() {
        before.max(1)
    } else {
        before
    };
    let mut reply = doc! {
        "numIndexesBefore": before as i32,
        "numIndexesAfter": after as i32,
    };
    if !all_existed {
        reply.insert("createdCollectionAutomatically", created_coll);
    }
    if ctx.replica_set_name.is_some() {
        let quorum = doc
            .get("commitQuorum")
            .cloned()
            .unwrap_or_else(|| Bson::String("votingMembers".into()));
        reply.insert("commitQuorum", quorum);
    }
    if any_created && any_existed {
        reply.insert("note", "index already exists");
    }
    // When every requested index already existed, mongod adds
    // `note: "all indexes already exist"` so drivers report a no-op (mongocxx's
    // `index_view::create_one` returns an empty optional off this).
    if all_existed {
        reply.insert("note", "all indexes already exist");
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// What mongod refuses in a `partialFilterExpression`.
enum PartialProblem {
    /// An operator barred from the context altogether: `(code, name, op)`.
    NotAllowed(i32, &'static str, &'static str),
    /// An expression a partial index cannot hold, as mongod prints the
    /// offending node of its parsed tree.
    Unsupported(String),
}

/// The first expression in `filter` a partial index does not support
/// (measured 8.2.11, 2026-10-10). Allowed: equality, `$gt` / `$gte` / `$lt` /
/// `$lte`, `$in`, `$exists: true`, `$type`, `$all`, the geo operators, and
/// `$and` / `$or` over those. The text after the operator name is mongod's
/// debug print of the node; it is reproduced for the single-value forms and
/// approximated beyond them.
fn partial_filter_problem(filter: &Document) -> Option<PartialProblem> {
    let show = |v: &Bson| argtypes::render_stage_value(v);
    for (key, value) in filter {
        match key.as_str() {
            "$expr" => return Some(PartialProblem::NotAllowed(224, "Location224", "$expr")),
            "$jsonSchema" => {
                return Some(PartialProblem::NotAllowed(
                    224,
                    "Location224",
                    "$jsonSchema",
                ))
            }
            "$where" => return Some(PartialProblem::NotAllowed(2, "BadValue", "$where")),
            "$text" => return Some(PartialProblem::NotAllowed(2, "BadValue", "$text")),
            "$and" | "$or" | "$nor" => {
                let clauses: Vec<&Document> = match value {
                    Bson::Array(a) => a.iter().filter_map(Bson::as_document).collect(),
                    _ => Vec::new(),
                };
                for clause in &clauses {
                    if let Some(p) = partial_filter_problem(clause) {
                        return Some(p);
                    }
                }
                if key == "$nor" {
                    let mut tree = String::from("$nor\n");
                    for clause in clauses {
                        for (f, v) in clause {
                            tree.push_str(&format!("    {f} $eq {}\n", show(v)));
                        }
                    }
                    return Some(PartialProblem::Unsupported(tree));
                }
            }
            _ if key.starts_with('$') => {}
            field => {
                let ops = match value {
                    Bson::RegularExpression(re) => {
                        return Some(PartialProblem::Unsupported(format!(
                            "{field} regex /{}/{}\n",
                            re.pattern, re.options
                        )))
                    }
                    Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => d,
                    _ => continue,
                };
                for (op, arg) in ops {
                    let tree = match op.as_str() {
                        "$ne" => format!("$not\n    {field} $eq {}\n", show(arg)),
                        "$nin" => {
                            let items: Vec<String> = match arg {
                                Bson::Array(a) => a.iter().map(&show).collect(),
                                other => vec![show(other)],
                            };
                            format!("$not\n    {field} $in [ {}]\n", items.join(" "))
                        }
                        "$not" => {
                            let mut tree = String::from("$not\n    $and\n");
                            if let Bson::Document(inner) = arg {
                                for (iop, iarg) in inner {
                                    tree.push_str(&format!(
                                        "        {field} {iop} {}\n",
                                        show(iarg)
                                    ));
                                }
                            }
                            tree
                        }
                        "$exists" if is_falsy(arg) => format!("$not\n    {field} exists\n"),
                        "$regex" => format!(
                            "{field} regex /{}/{}\n",
                            arg.as_str().unwrap_or_default(),
                            ops.get_str("$options").unwrap_or_default()
                        ),
                        "$mod" => match arg {
                            Bson::Array(a) if a.len() == 2 => {
                                format!("{field} mod {} % x == {}\n", show(&a[0]), show(&a[1]))
                            }
                            other => format!("{field} mod {}\n", show(other)),
                        },
                        "$size" => format!("{field} $size : {}\n", show(arg)),
                        "$elemMatch" => {
                            let mut tree = format!("{field} $elemMatch (value)\n");
                            if let Bson::Document(inner) = arg {
                                for (iop, iarg) in inner {
                                    tree.push_str(&format!("     {iop} {}\n", show(iarg)));
                                }
                            }
                            tree
                        }
                        "$bitsAllSet" | "$bitsAllClear" | "$bitsAnySet" | "$bitsAnyClear" => {
                            // mongod prints the bit POSITIONS a numeric mask sets.
                            let positions: Vec<String> = match arg {
                                Bson::Array(a) => a.iter().map(&show).collect(),
                                other => {
                                    let mask = as_i64(other).unwrap_or(0);
                                    (0..63)
                                        .filter(|bit| mask >> bit & 1 == 1)
                                        .map(|bit| bit.to_string())
                                        .collect()
                                }
                            };
                            format!("{field} {op}: [{}]\n", positions.join(", "))
                        }
                        _ => continue,
                    };
                    return Some(PartialProblem::Unsupported(tree));
                }
            }
        }
    }
    None
}

/// Why mongod refuses an index spec before building anything, or `None`.
/// Each message is what 8.2.11 answered (2026-10-10); all of these specs
/// used to be accepted and an index built from them.
fn index_spec_problem(spec: &Document) -> Option<CommandError> {
    let text = argtypes::render_stage_value(&Bson::Document(spec.clone()));
    // Some checks run after mongod has filled in the index version.
    let mut versioned = spec.clone();
    if !versioned.contains_key("v") {
        versioned.insert("v", 2_i32);
    }
    let versioned_text = argtypes::render_stage_value(&Bson::Document(versioned));
    let in_spec = |code: i32, name: &str, text: &str, why: &str| {
        CommandError::new(
            code,
            name,
            format!("Error in specification {text} :: caused by :: {why}"),
        )
    };
    let cannot = |why: &str| in_spec(67, "CannotCreateIndex", &text, why);
    let Some(Bson::Document(key)) = spec.get("key") else {
        return spec.get("key").is_none().then(|| {
            in_spec(
                9,
                "FailedToParse",
                &text,
                "The 'key' field is a required property of an index specification",
            )
        });
    };
    for (field, value) in key {
        if field.is_empty() {
            return Some(cannot("Index keys cannot be an empty field."));
        }
        if field.starts_with('$') && field != "$**" {
            return Some(cannot(
                "Index key contains an illegal field name: field name starts with '$'.",
            ));
        }
        match value {
            Bson::Boolean(_) => {
                return Some(cannot(
                    "Values in v:2 index key pattern cannot be of type bool. Only numbers > 0, \
                     numbers < 0, and strings are allowed.",
                ))
            }
            Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)
                if value.as_f64().or(as_i64(value).map(|n| n as f64)) == Some(0.0) =>
            {
                return Some(cannot("Values in the index key pattern cannot be 0."))
            }
            _ => {}
        }
    }
    if key.len() > 32 {
        return Some(CommandError::new(
            13103,
            "Location13103",
            "too many compound keys",
        ));
    }
    if let Some(v) = spec.get("v").and_then(as_i64) {
        if !(1..=2).contains(&v) {
            return Some(cannot(&format!(
                "Invalid index specification {text}; cannot create an index with v={v}"
            )));
        }
    }
    let name = spec.get_str("name").ok();
    if name == Some("") {
        return Some(in_spec(
            67,
            "CannotCreateIndex",
            &versioned_text,
            "index name cannot be empty",
        ));
    }
    if name == Some("*") {
        return Some(CommandError::new(
            2,
            "BadValue",
            "The index name '*' is not valid.",
        ));
    }
    // The `_id` index is `{_id: 1}` and takes no options of its own.
    if key.len() == 1 && key.contains_key("_id") {
        if as_i64(&key["_id"]) != Some(1) {
            return Some(CommandError::new(
                2,
                "BadValue",
                format!(
                    "The field 'key' for an _id index must be {{_id: 1}}, but got {}",
                    argtypes::render_stage_value(&Bson::Document(key.clone()))
                ),
            ));
        }
        if let Some(option) = spec
            .keys()
            .find(|k| !matches!(k.as_str(), "key" | "name" | "v" | "ns" | "collation"))
        {
            return Some(CommandError::new(
                197,
                "InvalidIndexSpecificationOption",
                format!(
                    "The field '{option}' is not valid for an _id index specification. \
                     Specification: {versioned_text}"
                ),
            ));
        }
    }
    if let Some(unknown) = spec.keys().find(|k| {
        !matches!(k.as_str(), "key" | "name") && !INDEX_SPEC_KNOWN_OPTIONS.contains(&k.as_str())
    }) {
        return Some(in_spec(
            197,
            "InvalidIndexSpecificationOption",
            &text,
            &format!(
                "The field '{unknown}' is not valid for an index specification. Specification: \
                 {text}"
            ),
        ));
    }
    if spec.contains_key("partialFilterExpression")
        && matches!(spec.get("sparse"), Some(Bson::Boolean(true)))
    {
        return Some(in_spec(
            67,
            "CannotCreateIndex",
            &versioned_text,
            "cannot mix \"partialFilterExpression\" and \"sparse\" options",
        ));
    }
    // A partial filter takes a narrow set of operators. The others were
    // accepted and an index built that the planner could never reason about.
    if let Some(Bson::Document(filter)) = spec.get("partialFilterExpression") {
        match partial_filter_problem(filter) {
            Some(PartialProblem::NotAllowed(code, name, op)) => {
                return Some(in_spec(
                    code,
                    name,
                    &text,
                    &format!("{op} is not allowed in this context"),
                ))
            }
            Some(PartialProblem::Unsupported(tree)) => {
                return Some(in_spec(
                    67,
                    "CannotCreateIndex",
                    &versioned_text,
                    &format!("Expression not supported in partial index: {tree}"),
                ))
            }
            None => {}
        }
    }
    // TTL: one field, and a non-negative 32-bit number of seconds.
    if let Some(seconds) = spec.get("expireAfterSeconds") {
        let ttl = |why: &str| {
            CommandError::new(
                67,
                "CannotCreateIndex",
                format!(". Index spec: {text} :: caused by :: {why}"),
            )
        };
        if matches!(seconds, Bson::Double(d) if d.is_nan()) {
            return Some(ttl("TTL index 'expireAfterSeconds' option must not be NaN"));
        }
        if let Some(n) = as_i64(seconds) {
            if matches!(
                seconds,
                Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)
            ) {
                if n < 0 {
                    return Some(ttl(
                        "TTL index 'expireAfterSeconds' option cannot be less than 0",
                    ));
                }
                if n > i32::MAX as i64 {
                    return Some(ttl(
                        "TTL index 'expireAfterSeconds' must be within the range of a 32-bit \
                         integer",
                    ));
                }
                if key.len() > 1 {
                    return Some(CommandError::new(
                        67,
                        "CannotCreateIndex",
                        format!(
                            "TTL indexes are single-field indexes, compound indexes do not \
                             support TTL. Index spec: {text}"
                        ),
                    ));
                }
            }
        }
    }
    None
}

/// The index plugin names mongod 8.2 recognises in a key pattern.
const KNOWN_INDEX_PLUGINS: &[&str] = &["2d", "2dsphere", "text", "hashed", "2dsphere_bucket"];

/// The fields `collMod` accepts, besides the generic command arguments.
const COLLMOD_FIELDS: &[&str] = &[
    "validator",
    "validationLevel",
    "validationAction",
    "index",
    "viewOn",
    "pipeline",
    "expireAfterSeconds",
    "changeStreamPreAndPostImages",
    "timeseries",
    "cappedSize",
    "cappedMax",
    "dryRun",
    "recordIdsReplicated",
    "timeseriesBucketsMayHaveMixedSchemaData",
];

/// mongod's refusal of a collection name it will not create (measured 8.2.11,
/// 2026-10-01). These used to be created.
fn invalid_collection_name(db: &str, coll: &str) -> Option<CommandError> {
    let err = |m: String| Some(CommandError::new(73, "InvalidNamespace", m));
    if coll.is_empty() {
        return err(format!("Invalid namespace specified: {db}"));
    }
    if coll.contains('\0') {
        return err("namespaces cannot have embedded null characters".to_string());
    }
    if coll.starts_with('.') {
        return err(format!("Collection names cannot start with '.': {coll}"));
    }
    if coll.contains('$') {
        return err(format!("Invalid collection name: {coll}"));
    }
    if let Some(rest) = coll.strip_prefix("system.") {
        let allowed = matches!(
            rest,
            "views" | "profile" | "js" | "users" | "roles" | "version"
        ) || rest.starts_with("buckets.");
        if !allowed {
            return err(format!("Invalid system namespace: {db}.{coll}"));
        }
    }
    None
}

/// mongod's refusal of a namespace a WRITE command (insert, update, delete,
/// findAndModify, createIndexes) may not touch -- measured 8.2.11,
/// 2026-10-05. These were accepted, and an insert created a collection mongod
/// cannot hold (`a$b`, `system.foo`, a 300-character name). Stricter than
/// `create` in two ways: the 255-character namespace limit, and the system
/// collections a client may not write (`system.views` / `system.profile`,
/// and `system.roles` / `system.version` outside `admin`).
/// `system.buckets.*` is left to the existing timeseries handling.
pub(crate) fn invalid_write_namespace(db: &str, coll: &str) -> Option<CommandError> {
    let err = |m: String| Some(CommandError::new(73, "InvalidNamespace", m));
    if let Some(e) = invalid_collection_name(db, coll) {
        if !coll.starts_with("system.") {
            return Some(e);
        }
    }
    if let Some(rest) = coll.strip_prefix("system.") {
        match rest {
            "js" | "users" => {}
            "roles" | "version" if db == "admin" => {}
            "views" | "profile" => return err(format!("cannot write to {db}.{coll}")),
            _ if rest.starts_with("buckets.") => {}
            _ => return err(format!("Invalid system namespace: {db}.{coll}")),
        }
    }
    let ns = format!("{db}.{coll}");
    if ns.len() > 255 {
        return err(format!(
            "Fully qualified namespace is too long. Namespace: {ns} Max: 255"
        ));
    }
    None
}

/// A UUID's canonical text from its 16 bytes.
fn uuid_text(bytes: &[u8]) -> Option<String> {
    let b: [u8; 16] = bytes.try_into().ok()?;
    Some(bson::uuid::Uuid::from_bytes(b).to_string())
}

/// mongod's spec rendering for a conflict message: `v` first, then the
/// options, then `key` and `name` (measured 8.2.11, 2026-10-01).
fn conflict_spec_text(spec: &Document) -> String {
    let mut out = Document::new();
    out.insert("v", 2i32);
    for (k, v) in spec {
        if !matches!(k.as_str(), "v" | "key" | "name" | "ns" | "collation")
            && !INDEX_CATALOG_ONLY.contains(&k.as_str())
        {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(k) = spec.get("key") {
        out.insert("key", k.clone());
    }
    if let Some(n) = spec.get("name") {
        out.insert("name", n.clone());
    }
    // The collation is the exception: it follows the name.
    if let Some(c) = spec.get("collation") {
        out.insert("collation", c.clone());
    }
    argtypes::render_stage_value(&Bson::Document(out))
}

/// A spec as mongod quotes it for an EQUIVALENT index: the options that do
/// not change which index it is come last, after the name.
fn equivalent_spec_text(spec: &Document) -> String {
    let mut out = Document::new();
    out.insert("v", 2i32);
    let trailing = |k: &str| matches!(k, "expireAfterSeconds" | "hidden");
    let listed =
        |k: &str| !matches!(k, "v" | "key" | "name" | "ns") && !INDEX_CATALOG_ONLY.contains(&k);
    for (k, v) in spec.iter().filter(|(k, _)| listed(k) && !trailing(k)) {
        out.insert(k.clone(), v.clone());
    }
    if let Some(k) = spec.get("key") {
        out.insert("key", k.clone());
    }
    if let Some(n) = spec.get("name") {
        out.insert("name", n.clone());
    }
    for (k, v) in spec.iter().filter(|(k, _)| trailing(k)) {
        out.insert(k.clone(), v.clone());
    }
    argtypes::render_stage_value(&Bson::Document(out))
}

/// The options that make two index specs different indexes.
/// Catalog bookkeeping that is not part of an index's options (and that
/// `listIndexes` already hides from clients).
pub(crate) const INDEX_CATALOG_ONLY: &[&str] = &[
    "entryFormat",
    "multikey",
    "collationKeys",
    "geoKeys",
    "pythonKeys",
];

fn index_options(spec: &Document) -> Document {
    spec.iter()
        .filter(|(k, _)| {
            !matches!(k.as_str(), "v" | "key" | "name" | "ns")
                && !INDEX_CATALOG_ONLY.contains(&k.as_str())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Two specs' documents compared as mongod compares them: field order matters,
/// but numbers are equal by VALUE -- a `{filename: 1.0}` index (mongocxx's
/// GridFS creates them that way) is the same index as `{filename: 1}`.
fn docs_equal_by_value(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && values_equal_by_value(va, vb))
}

fn values_equal_by_value(a: &Bson, b: &Bson) -> bool {
    let num = |v: &Bson| match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(d) => Some(*d),
        _ => None,
    };
    match (a, b) {
        (Bson::Document(x), Bson::Document(y)) => docs_equal_by_value(x, y),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

/// mongod's answer when a requested index collides with an existing one
/// (measured 8.2.11, 2026-10-01):
///
/// * same NAME, different key or options -> 86 `IndexKeySpecsConflict`, quoting
///   both specs;
/// * same KEY and options under another name -> 85 `IndexOptionsConflict`.
///   This one used to BUILD a second, duplicate index.
fn index_conflict(
    existing: &[Document],
    name: &str,
    key: &Document,
    spec: &Document,
) -> Option<CommandError> {
    let wanted = index_options(spec);
    for idx in existing {
        let same_name = idx.get_str("name").is_ok_and(|n| n == name);
        let same_key = idx
            .get_document("key")
            .is_ok_and(|k| docs_equal_by_value(k, key));
        let same_opts = docs_equal_by_value(&index_options(idx), &wanted);
        // Options that do not change WHICH index it is: the same name and key
        // with only these different is an equivalent index, 85.
        let identity = |d: &Document| -> Document {
            d.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "expireAfterSeconds" | "hidden"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        if same_name
            && same_key
            && !same_opts
            && docs_equal_by_value(&identity(&index_options(idx)), &identity(&wanted))
        {
            return Some(CommandError::new(
                85,
                "IndexOptionsConflict",
                format!(
                    "An equivalent index already exists with the same name but different \
                     options. Requested index: {}, existing index: {}",
                    equivalent_spec_text(spec),
                    equivalent_spec_text(idx),
                ),
            ));
        }
        if same_name && !(same_key && same_opts) {
            return Some(CommandError::new(
                86,
                "IndexKeySpecsConflict",
                format!(
                    "An existing index has the same name as the requested index. When index \
                     names are not specified, they are auto generated and can cause conflicts. \
                     Please refer to our documentation. Requested index: {}, existing index: {}",
                    conflict_spec_text(spec),
                    conflict_spec_text(idx),
                ),
            ));
        }
        if !same_name && same_key && same_opts {
            return Some(CommandError::new(
                85,
                "IndexOptionsConflict",
                format!(
                    "Index already exists with a different name: {}",
                    idx.get_str("name").unwrap_or("")
                ),
            ));
        }
    }
    None
}

/// `dropIndexes` — drop a named index, or all of them with `"*"`.
pub fn drop_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // A list of names is a third form `index` takes, beside a name and a key.
    let names: Option<Vec<String>> = match doc.get("index") {
        Some(Bson::Array(a)) => a.iter().map(|v| v.as_str().map(String::from)).collect(),
        _ => None,
    };
    if doc.get("index").is_none() {
        return Ok(CommandError::new(
            40414,
            "IDLFailedToParse",
            "BSON field 'dropIndexes.index' is missing but a required field",
        )
        .into_reply());
    }
    if names.is_none() {
        argtypes::require_index_name_or_key(doc, "index", "dropIndexes.index")?;
    }
    let coll = coll_arg(doc, "dropIndexes")?;
    let storage = ctx.storage()?;
    let exists = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?
        .iter()
        .any(|c| c == &coll);
    if !exists {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns not found {}.{coll}", ctx.db_name),
        )
        .into_reply());
    }
    let listed = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let is_time_series = storage
        .get_collection_options(&ctx.db_name, &coll)
        .is_ok_and(|o| o.contains_key("timeseries"));
    // A time-series collection lists no `_id` index, so none is counted.
    let before = listed.len().saturating_sub(usize::from(is_time_series));
    // The `_id` index is never dropped, by name, by key or in a list. By key
    // it used to be: `{dropIndexes: c, index: {_id: 1}}` removed it.
    let id_index = || {
        if is_time_series {
            CommandError::new(
                5979800,
                "Location5979800",
                "It is illegal to drop the clusteredIndex",
            )
            .into_reply()
        } else {
            CommandError::new(72, "InvalidOptions", "cannot drop _id index").into_reply()
        }
    };
    let not_found = |name: &str| {
        CommandError::new(
            27,
            "IndexNotFound",
            format!("index not found with name [{name}]"),
        )
        .into_reply()
    };
    let has = |name: &str| listed.iter().any(|ix| ix.get_str("name") == Ok(name));

    let mut reply = doc! { "nIndexesWas": before as i32 };
    match (names, doc.get("index")) {
        // Every name is checked before any index is dropped.
        (Some(names), _) => {
            if names.iter().any(|n| n == "_id_") {
                return Ok(id_index());
            }
            if let Some(missing) = names.iter().find(|n| !has(n)) {
                return Ok(not_found(missing));
            }
            for name in &names {
                storage
                    .drop_index(&ctx.db_name, &coll, name)
                    .map_err(command_error)?;
            }
        }
        (None, Some(Bson::String(s))) if s == "*" => {
            storage
                .drop_all_indexes(&ctx.db_name, &coll)
                .map_err(command_error)?;
            reply.insert("msg", "non-_id indexes dropped for collection");
        }
        (None, Some(Bson::String(name))) => {
            if let Some(e) = crate::nul_in_namespace("index name", name) {
                return Ok(e.into_reply());
            }
            if name == "_id_" {
                return Ok(id_index());
            }
            let existed = storage
                .drop_index(&ctx.db_name, &coll, name)
                .map_err(command_error)?;
            if !existed {
                return Ok(not_found(name));
            }
        }
        (None, Some(Bson::Document(key))) => {
            let same_key: Vec<&Document> = listed
                .iter()
                .filter(|idx| idx.get_document("key").map(|k| k == key).unwrap_or(false))
                .collect();
            // Several indexes can share a key when their collations differ;
            // a key then names none of them.
            if same_key.len() > 1 {
                let shown = |ix: &Document| {
                    let mut out = doc! { "v": 2i32 };
                    for (k, v) in ix {
                        if k != "v" && k != "ns" && !INDEX_CATALOG_ONLY.contains(&k.as_str()) {
                            out.insert(k.clone(), v.clone());
                        }
                    }
                    argtypes::render_stage_value(&Bson::Document(out))
                };
                return Ok(CommandError::new(
                    181,
                    "AmbiguousIndexKeyPattern",
                    format!(
                        "{} indexes found for key: {}, identify by name instead. Conflicting \
                         indexes: {}, {}",
                        same_key.len(),
                        argtypes::render_stage_value(&Bson::Document(key.clone())),
                        shown(same_key[0]),
                        shown(same_key[1]),
                    ),
                )
                .into_reply());
            }
            let named = same_key
                .first()
                .and_then(|idx| idx.get_str("name").ok().map(str::to_string));
            match named {
                Some(name) if name == "_id_" => return Ok(id_index()),
                Some(name) => {
                    storage
                        .drop_index(&ctx.db_name, &coll, &name)
                        .map_err(command_error)?;
                }
                None => {
                    return Ok(CommandError::new(
                        27,
                        "IndexNotFound",
                        format!(
                            "can't find index with key: {}",
                            argtypes::render_stage_value(&Bson::Document(key.clone()))
                        ),
                    )
                    .into_reply());
                }
            }
        }
        _ => {
            return Ok(CommandError::new(
                2,
                "BadValue",
                "dropIndexes requires an index name or '*'",
            )
            .into_reply())
        }
    }
    // A cursor whose plan reads a dropped index ends with it.
    if let Ok(cursors) = ctx.cursors() {
        let now: Vec<String> = storage
            .list_indexes(&ctx.db_name, &coll)
            .unwrap_or_default()
            .iter()
            .filter_map(|ix| ix.get_str("name").ok().map(str::to_string))
            .collect();
        let ns = format!("{}.{coll}", ctx.db_name);
        for gone in listed
            .iter()
            .filter_map(|ix| ix.get_str("name").ok())
            .filter(|name| !now.iter().any(|n| n == name))
        {
            cursors.kill_index(&ns, gone);
        }
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

pub fn drop_database(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    crate::stats_admin::drop_database_problem(doc)?;
    let storage = ctx.storage()?;
    // Every cursor open on one of its collections ends with that collection.
    if let Ok(cursors) = ctx.cursors() {
        for coll in storage.list_collections(&ctx.db_name).unwrap_or_default() {
            cursors.kill_namespace(
                &format!("{}.{coll}", ctx.db_name),
                &dropped_reason(storage, &ctx.db_name, &coll),
            );
        }
    }
    storage.drop_database(&ctx.db_name).map_err(command_error)?;
    // A replica-set member answers `{ok: 1}` alone (measured 8.2.11,
    // 2026-10-11, for an empty database, one with a collection and one with
    // a view). `dropped` is the standalone reply.
    if ctx.replica_set_name.is_some() {
        return Ok(doc! { "ok": 1.0 });
    }
    Ok(doc! { "dropped": ctx.db_name.clone(), "ok": 1.0 })
}

/// `renameCollection` — rename `renameCollection` (a full `db.coll` ns) to `to`.
pub fn rename_collection(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    argtypes::require_required_string(
        doc,
        "renameCollection",
        "renameCollection.renameCollection",
    )?;
    // The fields are PARSED first -- a wrong-typed `to` / `dropTarget` is
    // mongod's 14 / 40414 on any database (measured 8.2.11, 2026-10-07,
    // `arg_types_messages.py`) -- and only then is a non-`admin` database
    // refused (measured 2026-10-01).
    argtypes::require_required_string(doc, "to", "renameCollection.to")?;
    argtypes::require_bool_or_bindata(doc, "dropTarget", "renameCollection.dropTarget")?;
    if ctx.db_name != "admin" {
        return Err(CommandError::new(
            13,
            "Unauthorized",
            "renameCollection may only be run against the admin database.",
        ));
    }
    let src = match doc.get("renameCollection") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "renameCollection requires a string source namespace",
            ))
        }
    };
    let to = match doc.get("to") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(CommandError::new(
                2,
                "BadValue",
                "renameCollection requires a string 'to' namespace",
            )
            .into_reply())
        }
    };
    let drop_target = doc
        .get("dropTarget")
        .and_then(Bson::as_bool)
        .unwrap_or(false);
    if let Some(field) = argtypes::unknown_field(
        doc,
        &[
            "renameCollection",
            "to",
            "dropTarget",
            "stayTemp",
            "collectionUUID",
        ],
    ) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'renameCollection.{field}' is an unknown field."),
        ));
    }
    if let Some(refused) = crate::ddl_checks::rename(&src, &to) {
        return Err(refused);
    }
    let (src_db, src_coll) = split_ns(&src);
    let (dst_db, dst_coll) = split_ns(&to);

    let storage = ctx.storage()?;
    // mongod presents a time-series collection as a view, and a view is
    // neither renamed nor renamed onto.
    let is_time_series = |db: &str, coll: &str| {
        storage
            .get_collection_options(db, coll)
            .is_ok_and(|o| o.contains_key("timeseries"))
    };
    if is_time_series(&src_db, &src_coll) {
        return Ok(CommandError::new(
            166,
            "CommandNotSupportedOnView",
            format!("cannot rename view: {src}"),
        )
        .into_reply());
    }
    if is_time_series(&dst_db, &dst_coll) {
        return Ok(CommandError::new(
            48,
            "NamespaceExists",
            format!("a view already exists with that name: {to}"),
        )
        .into_reply());
    }
    // What the cursors open on either collection are told, worked out while
    // both still have their ids.
    let renamed = format!(
        "collection renamed from '{src}' to '{to}'. UUID {}",
        collection_uuid_text(storage, &src_db, &src_coll)
    );
    let replaced = dropped_reason(storage, &dst_db, &dst_coll);
    let (ok_, msg) = storage
        .rename_collection(&src_db, &src_coll, &dst_db, &dst_coll, drop_target)
        .map_err(command_error)?;
    if !ok_ {
        let m = msg.unwrap_or_else(|| "rename failed".to_string());
        // A missing source ("source namespace does not exist") is
        // NamespaceNotFound (26); an existing target ("target namespace exists")
        // is NamespaceExists (48). Check the source-missing phrasing first so a
        // bare "exist" substring doesn't misclassify "does not exist" as 48.
        let lower = m.to_lowercase();
        let (code, name) = if lower.contains("does not exist") || lower.contains("not found") {
            (26, "NamespaceNotFound")
        } else if lower.contains("exists") {
            (48, "NamespaceExists")
        } else {
            (26, "NamespaceNotFound")
        };
        // mongod's wording: no namespace after either, and a missing source
        // in the SAME database is named as a collection.
        let m = match (code, src_db == dst_db) {
            (48, _) => "target namespace exists".to_string(),
            (_, true) if m.to_lowercase().contains("source") => {
                format!("Source collection {src} does not exist")
            }
            (_, false) if m.to_lowercase().contains("source") => {
                "source namespace does not exist".to_string()
            }
            _ => m,
        };
        return Ok(CommandError::new(code, name, m).into_reply());
    }
    // A rename invalidates cursors open on the source (and the dropped target),
    // same as a drop — a later getMore then fails with CursorNotFound. Mirrors
    // commands.py::_rename_collection.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&src, &renamed);
        if drop_target {
            cursors.kill_namespace(&to, &replaced);
        }
    }
    Ok(doc! { "ok": 1.0 })
}

/// `collStats` for a time-series collection: its buckets, not its documents.
pub(crate) fn time_series_stats(coll: &str, ctx: &CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let index_sizes = storage
        .index_sizes(&ctx.db_name, coll)
        .map_err(command_error)?;
    let buckets = crate::ts_buckets::of_collection(storage, &ctx.db_name, coll)?;
    let bucket_size: i64 = buckets
        .iter()
        .map(|b| bson::to_vec(b).map(|v| v.len() as i64).unwrap_or(0))
        .sum();
    let n = buckets.len() as i64;
    let mut index_sizes = index_sizes;
    index_sizes.remove("_id_");
    let total_index_size: i64 = index_sizes.values().filter_map(as_i64).sum();
    Ok(doc! {
        "ns": format!("{}.{}", ctx.db_name, coll),
        "size": int_bson(bucket_size),
        "timeseries": {
            "bucketsNs": format!(
                "{}.{}{coll}", ctx.db_name, crate::ts_buckets::BUCKETS_PREFIX
            ),
            "bucketCount": int_bson(n),
            "avgBucketSize": int_bson(if n > 0 { bucket_size / n } else { 0 }),
        },
        "storageSize": int_bson(bucket_size),
        "capped": false,
        "nindexes": index_sizes.len() as i32,
        "totalIndexSize": int_bson(total_index_size),
        "indexSizes": index_sizes,
        "scaleFactor": 1_i32,
        "ok": 1.0,
    })
}

/// `serverStatus` — a minimal subset (host / version / process / uptime), plus a
/// live `metrics.cursor.open.total` so drivers can track cursor lifecycle
/// (mongo-php-driver `cursor-destruct-001` opens a batched cursor and asserts the
/// count rises by one, then returns to baseline after `killCursors`).
pub fn server_status(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // `CursorRegistry::len` prunes idle cursors then counts the live ones — the
    // count rises while a batched cursor is open and drops on killCursors.
    let open_cursors = ctx.cursors().map(|c| c.len()).unwrap_or(0) as i64;
    // Real counts when the server supplied them; zeros off-server (unit tests).
    let conns = ctx.conn_stats.unwrap_or_default();
    // Defaults to persistent when there is no storage (unit-test contexts),
    // matching the Python server's fallback rather than erroring the command.
    let persistent = ctx.storage().map(|s| !s.in_memory()).unwrap_or(true);
    let mut metrics = doc! {
        "cursor": {
            "open": { "total": open_cursors, "pinned": 0i64, "noTimeout": 0i64 },
        },
    };
    // The TTL monitor's counters. Drivers' and users' tests wait on `passes`
    // to know a sweep has happened instead of sleeping for one.
    if let Some((_, _, passes, deleted)) = ctx.storage().ok().and_then(|s| s.ttl_monitor()) {
        metrics.insert(
            "ttl",
            doc! {
                "deletedDocuments": deleted as i64,
                "invalidTTLIndexSkips": 0i64,
                "passes": passes as i64,
                "subPasses": passes as i64,
            },
        );
    }
    // The process's own numbers: its id, and how long since it first
    // answered a command. Each was a constant 0 before.
    let now = bson::DateTime::now();
    let up_millis =
        (now.timestamp_millis() - crate::handshake::process_started().timestamp_millis()).max(0);
    let host = match ctx.server_address.as_ref() {
        Some((host, port)) => format!("{host}:{port}"),
        None => "secantus".to_string(),
    };
    // Counted per command as mongod counts them; int64, as mongod sends them.
    let counted = ctx
        .top_stats
        .as_ref()
        .map(|s| s.opcounters())
        .unwrap_or_default();
    let opcounters: Document = crate::topstats::OPCOUNTERS
        .iter()
        .zip(counted)
        .map(|(name, n)| (name.to_string(), Bson::Int64(n)))
        .collect();
    let network = ctx
        .top_stats
        .as_ref()
        .map(|s| s.network())
        .unwrap_or_default();
    Ok(doc! {
        "host": host,
        "version": crate::SERVER_VERSION,
        "process": "mongod",
        "pid": Bson::Int64(i64::from(std::process::id())),
        "uptime": (up_millis / 1000) as f64,
        "uptimeMillis": Bson::Int64(up_millis),
        "uptimeEstimate": Bson::Int64(up_millis / 1000),
        "localTime": now,
        "metrics": metrics,
        // mongo-c-driver's `/Client/exhaust_cursor/{single,pool}` read
        // `connections.totalCreated` off serverStatus to check the connection
        // pool wasn't cleared. Omitting the section made those fail with
        // "'connections.totalCreated' field not found" — a serverStatus gap
        // that looked like an exhaust-cursor bug. Mirrors the Python server's
        // zeroed block; SecantusDB keeps no pool counters.
        // Int32, not Int64: libmongoc reads these with `bson_lookup_int32`,
        // which type-checks rather than coercing ("'connections.totalCreated'
        // is not a int32"). The Python server emits plain ints, which encode
        // as Int32, so it never hit this.
        "connections": {
            "current": conns.current as i32,
            "available": 0i32,
            "totalCreated": conns.total_created as i32,
        },
        "opcounters": opcounters,
        // What the server has read and written, counted off the sockets.
        "network": {
            "bytesIn": Bson::Int64(network[1]),
            "bytesOut": Bson::Int64(network[2]),
            "numRequests": Bson::Int64(network[0]),
        },
        // Storage-engine identity. Drivers gate real behaviour on this:
        // mongo-php-library's `skipIfTransactionsNotSupported` reads
        // `storageEngine.name` and throws "Could not determine server storage
        // engine" when the key is absent, turning ~27 transaction tests into
        // ERRORs instead of the clean skip the helper intends. Reporting
        // "wiredTiger" is honest — SecantusDB is WiredTiger-backed, the same
        // engine mongod uses. Kept byte-identical to the Python server's
        // `_storage_engine_section`.
        "storageEngine": {
            "name": "wiredTiger",
            "supportsCommittedReads": true,
            "supportsPendingDrops": true,
            "supportsSnapshotReadConcern": true,
            "readOnly": false,
            "persistent": persistent,
            "backupCursorOpen": false,
        },
        // Categorical self-identification: real mongod never has this key.
        // Tooling (the conformance-gauge tripwire, ad-hoc smoke scripts)
        // checks it to prove it's talking to SecantusDB rather than an
        // accidental real MongoDB on the same address. The Python server
        // reports `server: "python"`.
        "secantus": { "server": "rust", "version": env!("CARGO_PKG_VERSION") },
        "ok": 1.0,
    })
}

/// `currentOp` — mongod's in-flight-operation introspection. SecantusDB runs
/// commands synchronously and keeps no per-op registry, so the only operation
/// "in progress" is the `currentOp` request itself. We emit one synthetic
/// `inprog` entry carrying this connection's driver `clientMetadata` (captured
/// from the handshake `client` doc), which is what the drivers' handshake-
/// metadata tests read back. A client filter (e.g. `command.currentOp` /
/// `$ownOps`) is accepted but not applied — the single self-op already matches
/// the introspection queries the drivers issue.
pub fn current_op(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let client_metadata = ctx
        .conn_auth
        .as_ref()
        .and_then(|a| a.lock().ok())
        .and_then(|g| g.client_metadata.clone());

    let mut op = doc! {
        "type": "op",
        "host": "secantus",
        "desc": "conn",
        "connectionId": Bson::Int64(ctx.connection_id),
        "active": true,
        "op": "command",
        "ns": format!("{}.$cmd", ctx.db_name),
        "command": doc.clone(),
        "opid": Bson::Int64(ctx.connection_id),
        "secs_running": Bson::Int64(0),
        "microsecs_running": Bson::Int64(0),
    };
    if let Some(meta) = client_metadata {
        op.insert("clientMetadata", meta);
    }

    // Every field that is not the command's own is a filter over the
    // operations (`{currentOp: 1, op: "query"}`).
    let filter: Document = doc
        .iter()
        .skip(1)
        .filter(|(key, _)| {
            !matches!(key.as_str(), "$all" | "$ownOps" | "$truncateOps")
                && !crate::params::is_generic_arg(key)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let shown = filter.is_empty()
        || secantus_core::query::matches(&op, &filter, &Document::new(), None).unwrap_or(false);
    let inprog: Vec<Bson> = if shown {
        vec![Bson::Document(op)]
    } else {
        Vec::new()
    };
    Ok(doc! { "inprog": inprog, "ok": 1.0 })
}

/// `validate` — a collection checked against its indexes.
///
/// The store reads every document and compares each index with the entries
/// its documents should have, both ways, and the `_id` index with the
/// documents (`Storage::check_collection`). `valid` is false, with the
/// counts in `errors`, when a document cannot be read, an entry is missing
/// or an entry belongs to no document. Until 2026-10-13 this answered
/// `valid: true` without looking and reported one key per document for
/// every index. `full` and `background` change how mongod scans and not the
/// verdict, so they are taken and change nothing here; nothing is repaired.
pub fn validate(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = match doc.get("validate") {
        Some(Bson::String(s)) => s.clone(),
        _ => {
            return Ok(
                CommandError::new(14, "TypeMismatch", "validate requires a collection name")
                    .into_reply(),
            )
        }
    };
    let storage = ctx.storage()?;
    // `system.views` is what mongod keeps its view definitions in. Here they
    // are in the catalog, and it is validated as `find` and `listCollections`
    // show it: one record a definition, under its `_id` index.
    let exists = storage
        .collection_exists(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let view_definitions = if coll == "system.views" && !exists {
        Some(crate::ts_buckets::view_definitions(storage, &ctx.db_name)?)
            .filter(|definitions| !definitions.is_empty())
    } else {
        None
    };
    if !exists && view_definitions.is_none() {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!(
                "Collection '{}.{}' does not exist to validate.",
                ctx.db_name, coll
            ),
        )
        .into_reply());
    }
    // An option is on for any value but null, false and zero (a string too).
    let on = |option: &str| match doc.get(option) {
        None | Some(Bson::Null) | Some(Bson::Boolean(false)) => false,
        Some(v @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => crate::util::truthy(v),
        Some(_) => true,
    };
    if on("repair") && ctx.replica_set_name.is_some() {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "Running the validate command with { repair: true } can only be performed in \
             standalone mode.",
        ));
    }
    if on("metadata")
        && ["full", "repair", "background", "checkBSONConformance"]
            .iter()
            .any(|other| on(other))
    {
        return Err(CommandError::new(
            72,
            "InvalidOptions",
            "Running the validate command with { metadata: true } is not supported with any \
             other options",
        ));
    }
    // mongod rejects full+background together (full needs an exclusive scan).
    if bool_field(doc, "full", false) && bool_field(doc, "background", false) {
        return Ok(CommandError::new(
            72,
            "InvalidOptions",
            "Running the validate command with both { background: true } and { full: true } is \
             not supported.",
        )
        .into_reply());
    }
    if let Some(definitions) = view_definitions {
        let n = int_bson(definitions.len() as i64);
        return Ok(doc! {
            "valid": true,
            "repaired": false,
            "repairMode": "None",
            "ns": format!("{}.system.views", ctx.db_name),
            "uuid": crate::ts_buckets::derived_uuid(ctx.db_name.as_bytes(), 2),
            "warnings": Bson::Array(vec![]),
            "errors": Bson::Array(vec![]),
            "extraIndexEntries": Bson::Array(vec![]),
            "missingIndexEntries": Bson::Array(vec![]),
            "nInvalidDocuments": 0_i32,
            "nNonCompliantDocuments": 0_i32,
            "nrecords": n.clone(),
            "corruptRecords": Bson::Array(vec![]),
            "nIndexes": 1_i32,
            "keysPerIndex": {"_id_": n},
            "indexDetails": {
                "_id_": {"valid": true, "spec": {"v": 2, "key": {"_id": 1}, "name": "_id_"}},
            },
            "ok": 1.0,
        });
    }
    // A time-series collection is validated as its buckets: mongod counts
    // them, names their namespace and shows each index as it lies on them.
    let options = storage
        .get_collection_options(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let layout = crate::ts_buckets::Layout::of(&options);
    let nrecords = match &layout {
        Some(_) => crate::ts_buckets::of_collection(storage, &ctx.db_name, &coll)?.len() as i64,
        None => storage
            .count_matching(&ctx.db_name, &coll, &Document::new())
            .map_err(command_error)? as i64,
    };
    // The check itself: every document read, and each index compared with
    // the entries its documents should have. A store that cannot check
    // itself (a test double) reports what it is told.
    let check = storage
        .check_collection(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let checked: std::collections::HashMap<String, Document> = check
        .as_ref()
        .and_then(|c| c.get_array("indexes").ok())
        .into_iter()
        .flatten()
        .filter_map(Bson::as_document)
        .filter_map(|ix| Some((ix.get_str("name").ok()?.to_string(), ix.clone())))
        .collect();
    let count = |ix: &Document, field: &str| ix.get_i64(field).unwrap_or(0);
    let undecodable = check.as_ref().map_or(0, |c| count(c, "undecodable"));
    let missing: i64 = checked.values().map(|ix| count(ix, "missing")).sum();
    let extra: i64 = checked.values().map(|ix| count(ix, "extra")).sum();
    let mut errors: Vec<Bson> = Vec::new();
    if undecodable > 0 {
        errors.push(Bson::String(format!(
            "Detected {undecodable} documents that could not be read."
        )));
    }
    if missing > 0 {
        errors.push(Bson::String(format!(
            "Detected {missing} missing index entries."
        )));
    }
    if extra > 0 {
        errors.push(Bson::String(format!(
            "Detected {extra} extra index entries."
        )));
    }
    let indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let mut keys_per_index = Document::new();
    let mut index_details = Document::new();
    let mut n_indexes = 0i32;
    for ix in &indexes {
        let Ok(name) = ix.get_str("name") else {
            continue;
        };
        if layout.is_some() && name == "_id_" {
            continue;
        }
        let mut spec = doc! { "v": 2 };
        if let Ok(key) = ix.get_document("key") {
            spec.insert(
                "key",
                match &layout {
                    Some(l) => crate::ts_buckets::bucket_key(key, l),
                    None => key.clone(),
                },
            );
        }
        spec.insert("name", name);
        for (option, value) in ix {
            if !matches!(option.as_str(), "v" | "key" | "name" | "ns")
                && !INDEX_CATALOG_ONLY.contains(&option.as_str())
            {
                spec.insert(option.clone(), value.clone());
            }
        }
        // The keys mongod holds for these documents (an array gives one
        // per element), and whether the index holds what it should. A
        // time-series collection's are shown as they lie on its buckets.
        let found = checked.get(name);
        let keys = match (found, &layout) {
            (Some(ix), None) => count(ix, "keys"),
            _ => nrecords,
        };
        let sound = found.is_none_or(|ix| count(ix, "missing") == 0 && count(ix, "extra") == 0);
        keys_per_index.insert(name, int_bson(keys));
        index_details.insert(name, doc! { "valid": sound, "spec": spec });
        n_indexes += 1;
    }
    let ns = match &layout {
        Some(_) => format!(
            "{}.{}{coll}",
            ctx.db_name,
            crate::ts_buckets::BUCKETS_PREFIX
        ),
        None => format!("{}.{}", ctx.db_name, coll),
    };
    // mongod's field order (measured 8.2.11).
    let mut reply = doc! {
        "valid": errors.is_empty(),
        "repaired": false,
        "repairMode": "None",
        "ns": ns,
    };
    if let Ok(uuid) = storage.collection_uuid(&ctx.db_name, &coll) {
        if uuid.len() == 16 {
            let uuid = match &layout {
                Some(_) => crate::ts_buckets::derived_uuid(&uuid, 1),
                None => Bson::Binary(bson::Binary {
                    subtype: bson::spec::BinarySubtype::Uuid,
                    bytes: uuid,
                }),
            };
            reply.insert("uuid", uuid);
        }
    }
    reply.extend(doc! {
        "warnings": Bson::Array(vec![]),
        "errors": errors,
        "extraIndexEntries": Bson::Array(vec![]),
        "missingIndexEntries": Bson::Array(vec![]),
        "nInvalidDocuments": int_bson(undecodable),
        "nNonCompliantDocuments": 0_i32,
        "nrecords": int_bson(nrecords),
        "corruptRecords": Bson::Array(vec![]),
        "nIndexes": n_indexes,
        "keysPerIndex": keys_per_index,
        "indexDetails": index_details,
        "ok": 1.0,
    });
    Ok(reply)
}

/// `profile` -- read or change the database's profiling settings
/// ([`crate::profiler`]). The reply carries the settings as they were:
/// `was` (the level), the server-wide `slowms` and `sampleRate`, the
/// `filter` when one was set, and mongod's `note` whenever a filter was set
/// or is being given.
///
/// mongod's reading of the arguments (8.2.11): the level is any number,
/// truncated; one outside 0..=2 leaves the level alone but still applies
/// `slowms`, `sampleRate` and `filter`. `slowms` is any number, truncated and
/// not range-checked; `sampleRate` is a number from 0 to 1; `filter` is a
/// document or the string "unset" (null is refused). Setting level 1 or 2
/// creates `system.profile` at once.
pub fn profile(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db = ctx.db_name.clone();
    let profiler = ctx.profiler.get_or_insert_with(Default::default).clone();
    let slow_ops = ctx.slow_ops.get_or_insert_with(Default::default).clone();
    let prev = profiler.settings(&db);
    let number = |field: &str| -> Result<Option<f64>, CommandError> {
        match doc.get(field) {
            None => Ok(None),
            Some(Bson::Null) if field != "profile" => Ok(None),
            Some(Bson::Int32(n)) => Ok(Some(f64::from(*n))),
            Some(Bson::Int64(n)) => Ok(Some(*n as f64)),
            Some(Bson::Double(d)) => Ok(Some(*d)),
            Some(v @ Bson::Decimal128(_)) => Ok(v.to_string().parse::<f64>().ok()),
            Some(Bson::Null) => Err(CommandError::new(
                40414,
                "IDLFailedToParse",
                "BSON field 'profile.profile' is missing but a required field",
            )),
            Some(v) => Err(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'profile.{field}' is the wrong type '{}', expected types \
                     '[decimal, double, int, long]'",
                    secantus_core::query::bson_type_name(v)
                ),
            )),
        }
    };
    let level = number("profile")?.map_or(-1, |n| n.trunc() as i64);
    let slowms = number("slowms")?.map(|n| n.trunc() as i32);
    let rate = number("sampleRate")?;
    if rate.is_some_and(|r| !(0.0..=1.0).contains(&r)) {
        return Err(CommandError::new(
            2,
            "BadValue",
            "'sampleRate' must be between 0.0 and 1.0 inclusive",
        ));
    }
    let filter = match doc.get("filter") {
        None => None,
        Some(Bson::String(word)) if word == "unset" => Some(None),
        Some(Bson::Document(f)) => Some(Some(std::sync::Arc::new(
            crate::profiler::filter::ProfileFilter::parse(f)?,
        ))),
        Some(_) => {
            return Err(CommandError::new(
                2,
                "BadValue",
                "Expected an object, or the string 'unset'.",
            ));
        }
    };
    let mut was = doc! {
        "was": prev.level,
        "slowms": slow_ops.slow_ms(),
        "sampleRate": slow_ops.sample_rate(),
    };
    if let Some(f) = &prev.filter {
        was.insert("filter", f.serialized().clone());
    }
    if prev.filter.is_some() || filter.is_some() {
        was.insert(
            "note",
            "When a filter expression is set, slowms and sampleRate are not used for \
             profiling and slow-query log lines.",
        );
    }
    was.insert("ok", 1.0);
    let level = (0..=2).contains(&level).then_some(level as i32);
    if slowms.is_some() || rate.is_some() {
        slow_ops.set(
            slowms.unwrap_or_else(|| slow_ops.slow_ms()),
            rate.unwrap_or_else(|| slow_ops.sample_rate()),
        );
    }
    if level.is_some() || filter.is_some() {
        profiler.set(&db, level, filter);
    }
    if level.is_some_and(|l| l > 0) {
        let storage = ctx.storage()?;
        if !storage
            .collection_exists(&db, crate::profiler::PROFILE_COLLECTION)
            .map_err(command_error)?
        {
            storage
                .create_collection_with_options(
                    &db,
                    crate::profiler::PROFILE_COLLECTION,
                    &doc! {
                        "capped": true,
                        "size": crate::profiler::PROFILE_COLLECTION_BYTES,
                    },
                )
                .map_err(command_error)?;
        }
    }
    Ok(was)
}

/// Options mongod's index-spec IDL accepts, plus the legacy / deprecated forms
/// drivers still emit. Anything outside this set is an unknown field.
/// Mirrors `commands._INDEX_SPEC_KNOWN_OPTIONS` — keep the two in step.
const INDEX_SPEC_KNOWN_OPTIONS: &[&str] = &[
    // Geometric / vector indexes.
    "2dsphereIndexVersion",
    "bits",
    "min",
    "max",
    // Wildcard.
    "wildcardProjection",
    // Standard knobs.
    "unique",
    "sparse",
    "hidden",
    "background",
    "expireAfterSeconds",
    "partialFilterExpression",
    "collation",
    "storageEngine",
    // Text — accepted on the wire even though text indexes are unsupported
    // (storage rejects them with CreateIndexUnsupported).
    "weights",
    "default_language",
    "language_override",
    "textIndexVersion",
    // Index format version + namespace (legacy drivers).
    "v",
    "ns",
    // Haystack (deprecated).
    "bucketSize",
    // Removed in MongoDB 3.0; modern mongod accepts and silently ignores it,
    // so a unique index over duplicate data still fails on the duplicate
    // rather than on an unknown-field error.
    "dropDups",
];

/// Top-level options the `create` command accepts, plus the wire-envelope
/// fields a driver may attach. Mirrors `commands._CREATE_KNOWN_OPTIONS`.
const CREATE_KNOWN_OPTIONS: &[&str] = &[
    "create",
    "capped",
    "size",
    "max",
    "validator",
    "validationAction",
    "validationLevel",
    "viewOn",
    "pipeline",
    "collation",
    "expireAfterSeconds",
    "timeseries",
    "clusteredIndex",
    "changeStreamPreAndPostImages",
    "storageEngine",
    "indexOptionDefaults",
    "writeConcern",
    "comment",
    "maxTimeMS",
    // mongorestore sends the source collection's full `_id_` spec.
    "idIndex",
    // Legacy / deprecated but tolerated.
    "autoIndexId",
    "flags",
    // Non-`$`-prefixed envelope fields ( `$`-prefixed keys are accepted
    // unconditionally by the caller).
    "lsid",
    "txnNumber",
    "autocommit",
    "startTransaction",
    "readConcern",
    "apiVersion",
    "apiStrict",
    "apiDeprecationErrors",
];

/// The first field of `doc` outside `known`, ignoring `$`-prefixed envelope
/// keys. mongod surfaces an unknown field as `Location40415` (IDLUnknownField)
/// rather than ignoring it, and driver suites rely on that: mongo-ruby-driver's
/// "a failed operation using a session" shared specs provoke it deliberately by
/// passing `invalid: true` and asserting an `OperationFailure`.
fn first_unknown_field(doc: &Document, known: &[&str]) -> Option<String> {
    doc.keys()
        .find(|k| !k.starts_with('$') && !known.contains(&k.as_str()))
        .cloned()
}

/// Split a `db.coll` namespace into `(db, coll)`.
fn split_ns(ns: &str) -> (String, String) {
    match ns.split_once('.') {
        Some((d, c)) => (d.to_string(), c.to_string()),
        None => (String::new(), ns.to_string()),
    }
}

/// `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` — Atlas Search
/// index management, an Atlas-only feature.
///
/// A real non-Atlas mongod *registers* these commands and fails them at
/// execution with a message naming Atlas; the driver index-management spec
/// tests assert only that the error mentions Atlas. Leaving them unregistered
/// returns `CommandNotFound` (59) instead, which is what
/// mongo-c-driver's `/index-management/{update,drop}SearchIndex` caught. The
/// message is shared with the `$listSearchIndexes` stage so the two stay in
/// lockstep. Mirrors `commands._search_index_not_supported`.
pub fn search_index_not_supported(_doc: &Document, _ctx: &mut CommandContext) -> HandlerResult {
    Ok(CommandError::new(
        115,
        "CommandNotSupported",
        crate::aggregate::SEARCH_INDEX_ATLAS_MSG,
    )
    .into_reply())
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use bson::doc;

    fn ctx() -> CommandContext {
        let mut c = CommandContext::new(1);
        c.db_name = "testdb".to_string();
        c
    }

    fn err_of(reply: &Document) -> (i32, String, String) {
        (
            reply.get_i32("code").unwrap_or_default(),
            reply.get_str("codeName").unwrap_or_default().to_string(),
            reply.get_str("errmsg").unwrap_or_default().to_string(),
        )
    }

    /// mongo-c-driver's `/index-management/{update,drop}SearchIndex` assert the
    /// error names Atlas. Leaving the commands unregistered returned
    /// `CommandNotFound` (59) instead.
    #[test]
    fn search_index_commands_report_atlas_not_command_not_found() {
        for name in [
            "createSearchIndexes",
            "updateSearchIndex",
            "dropSearchIndex",
        ] {
            assert!(
                crate::lookup_for_test(name).is_some(),
                "{name} must be registered, not CommandNotFound"
            );
        }
        let reply = search_index_not_supported(&doc! {"dropSearchIndex": "c"}, &mut ctx()).unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (115, "CommandNotSupported"));
        assert!(
            msg.contains("Atlas"),
            "the driver specs assert on 'Atlas': {msg}"
        );
    }

    /// mongo-ruby-driver's "a failed operation using a session" shared specs
    /// pass `invalid: true` and assert an `OperationFailure`; silently
    /// accepting the unknown field made them fail.
    #[test]
    fn create_rejects_an_unknown_top_level_option() {
        let reply = create(&doc! {"create": "c", "invalid": true}, &mut ctx()).unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (40415, "IDLUnknownField"));
        assert!(msg.contains("create.invalid"), "{msg}");
    }

    #[test]
    fn create_still_accepts_every_known_option_and_the_wire_envelope() {
        // A `$`-prefixed envelope key and the non-`$` ones must pass through.
        let d = doc! {
            "create": "c", "capped": true, "size": 4096_i64, "max": 512_i64,
            "lsid": {"id": "x"}, "$db": "testdb", "writeConcern": {"w": 1},
        };
        assert!(
            first_unknown_field(&d, CREATE_KNOWN_OPTIONS).is_none(),
            "known options must not trip the unknown-field check"
        );
    }

    /// End to end through `createIndexes`, which is what the Ruby spec drives:
    /// `view.create_one({random: 1}, invalid: true)`.
    #[test]
    fn create_indexes_rejects_an_unknown_spec_option() {
        let mut c = ctx();
        c = c.with_storage(std::sync::Arc::new(FakeStorage));
        let reply = create_indexes(
            &doc! {
                "createIndexes": "specs",
                "indexes": [{"key": {"random": 1}, "name": "random_1", "invalid": true}],
            },
            &mut c,
        )
        .unwrap();
        let (code, name, msg) = err_of(&reply);
        // mongod 8.2.11 answers 197 (this asserted 40415 until 2026-10-10).
        assert_eq!(
            (code, name.as_str()),
            (197, "InvalidIndexSpecificationOption")
        );
        assert!(msg.contains("invalid"), "{msg}");
    }

    #[test]
    fn create_indexes_accepts_a_valid_spec() {
        let mut c = ctx();
        c = c.with_storage(std::sync::Arc::new(FakeStorage));
        let reply = create_indexes(
            &doc! {
                "createIndexes": "specs",
                "indexes": [{"key": {"a": 1}, "name": "a_1", "unique": true}],
            },
            &mut c,
        )
        .unwrap();
        assert_eq!(reply.get_f64("ok").unwrap_or(0.0), 1.0, "{reply:?}");
    }

    /// mongo-php-library's `skipIfTransactionsNotSupported` reads
    /// `storageEngine.name`, and throws "Could not determine server storage
    /// engine" when it is absent — erroring ~27 transaction tests rather than
    /// skipping them. Must stay byte-identical to the Python server's
    /// `_storage_engine_section`.
    #[test]
    fn server_status_reports_wiredtiger_storage_engine() {
        let mut c = ctx();
        c = c.with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let reply = server_status(&doc! {"serverStatus": 1}, &mut c).unwrap();
        let engine = reply.get_document("storageEngine").expect("storageEngine");
        assert_eq!(engine.get_str("name").unwrap(), "wiredTiger");
        assert!(engine.get_bool("supportsCommittedReads").unwrap());
        assert!(engine.get_bool("supportsSnapshotReadConcern").unwrap());
        assert!(!engine.get_bool("readOnly").unwrap());
        // No storage attached (unit context) falls back to persistent, matching
        // the Python server rather than failing the command.
        assert!(engine.get_bool("persistent").unwrap());
    }

    /// mongo-c-driver's `/Client/exhaust_cursor/{single,pool}` read
    /// `connections.totalCreated` to check the pool wasn't cleared. Its absence
    /// failed them with "field not found", which read as an exhaust-cursor bug.
    #[test]
    fn server_status_carries_the_sections_drivers_read() {
        let mut c = ctx();
        c = c.with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let reply = server_status(&doc! {"serverStatus": 1}, &mut c).unwrap();
        let conns = reply.get_document("connections").expect("connections");
        // Int32 specifically — libmongoc type-checks with bson_lookup_int32
        // rather than coercing, so an Int64 zero fails just as hard as a
        // missing field.
        for f in ["totalCreated", "current", "available"] {
            assert!(
                matches!(conns.get(f), Some(bson::Bson::Int32(_))),
                "connections.{f} must be Int32: {conns:?}"
            );
        }

        // With real counters attached, the reported values are those — not
        // zeros. The exhaust tests read `totalCreated` before and after opening
        // a cursor and require it to have risen, so a constant is not enough.
        let mut c2 = ctx();
        c2 = c2
            .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()))
            .with_conn_stats(crate::ConnStats {
                current: 3,
                total_created: 7,
            });
        let reply2 = server_status(&doc! {"serverStatus": 1}, &mut c2).unwrap();
        let conns2 = reply2.get_document("connections").unwrap();
        assert_eq!(conns2.get_i32("totalCreated").unwrap(), 7);
        assert_eq!(conns2.get_i32("current").unwrap(), 3);
        assert!(reply.get_document("opcounters").is_ok());
        assert!(reply.get_document("network").is_ok());
    }

    /// mongo-ruby-driver's `Collection#indexes when a session is provided` uses
    /// `batch_size: -100` as its deliberately-failing operation.
    #[test]
    fn list_indexes_rejects_a_negative_batch_size() {
        let mut c = ctx();
        c = c
            .with_storage(std::sync::Arc::new(FakeStorage))
            .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
        let err = list_indexes(
            &doc! {"listIndexes": "specs", "cursor": {"batchSize": -100_i32}},
            &mut c,
        )
        .unwrap_err();
        assert_eq!((err.code, err.code_name.as_str()), (2, "BadValue"));
        assert_eq!(
            err.errmsg,
            "BSON field 'batchSize' value must be >= 0, actual value '-100'"
        );
    }

    #[test]
    fn list_indexes_still_accepts_a_zero_or_positive_batch_size() {
        for bs in [0_i32, 2_i32] {
            let mut c = ctx();
            c = c
                .with_storage(std::sync::Arc::new(FakeStorage))
                .with_cursors(std::sync::Arc::new(crate::CursorRegistry::new()));
            let reply = list_indexes(
                &doc! {"listIndexes": "specs", "cursor": {"batchSize": bs}},
                &mut c,
            )
            .unwrap();
            assert_eq!(
                reply.get_f64("ok").unwrap_or(0.0),
                1.0,
                "batchSize {bs}: {reply:?}"
            );
        }
    }

    /// Minimal in-memory `Storage`: only the methods without a default impl.
    struct FakeStorage;

    impl crate::Storage for FakeStorage {
        fn insert(
            &self,
            _db: &str,
            _coll: &str,
            _docs: Vec<Vec<u8>>,
            _ordered: bool,
        ) -> Result<(usize, Vec<Document>), crate::StorageError> {
            Ok((0, Vec::new()))
        }
        fn update_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _update: &Document,
            _multi: bool,
            _upsert: bool,
        ) -> Result<crate::UpdateOutcome, crate::StorageError> {
            Ok(crate::UpdateOutcome::default())
        }
        fn delete_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _limit: usize,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn count_matching(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
        ) -> Result<usize, crate::StorageError> {
            Ok(0)
        }
        fn find(
            &self,
            _db: &str,
            _coll: &str,
            _filter: &Document,
            _sort: Option<&Document>,
            _hint: Option<crate::storage::RawHint<'_>>,
        ) -> Result<Vec<Vec<u8>>, crate::StorageError> {
            Ok(Vec::new())
        }
        /// A non-empty result is what marks the namespace as existing — an
        /// empty one is how `list_indexes` detects NamespaceNotFound.
        fn list_indexes(
            &self,
            _db: &str,
            _coll: &str,
        ) -> Result<Vec<Document>, crate::StorageError> {
            Ok(vec![doc! {"v": 2, "key": {"_id": 1}, "name": "_id_"}])
        }
    }

    #[test]
    fn index_spec_accepts_the_documented_options() {
        for k in [
            "unique",
            "sparse",
            "hidden",
            "background",
            "expireAfterSeconds",
            "partialFilterExpression",
            "collation",
            "storageEngine",
            "weights",
            "v",
            "ns",
            "bucketSize",
            "dropDups",
            "2dsphereIndexVersion",
            "bits",
            "min",
            "max",
            "wildcardProjection",
        ] {
            let d = doc! { k: 1 };
            assert!(
                first_unknown_field(&d, INDEX_SPEC_KNOWN_OPTIONS).is_none(),
                "{k} is a real mongod index option and must be accepted"
            );
        }
    }
}

/// `invalid_write_namespace`, each answer measured on mongod 8.2.11
/// (2026-10-05) by `tools/probes/write_and_sort_validation.py`.
#[cfg(test)]
mod write_namespace_tests {
    use super::invalid_write_namespace;

    fn msg(db: &str, coll: &str) -> Option<String> {
        invalid_write_namespace(db, coll).map(|e| {
            assert_eq!(e.code, 73);
            e.errmsg
        })
    }

    #[test]
    fn refuses_what_mongod_will_not_write() {
        assert_eq!(
            msg("p", "a$b").as_deref(),
            Some("Invalid collection name: a$b")
        );
        assert_eq!(
            msg("p", ".a").as_deref(),
            Some("Collection names cannot start with '.': .a")
        );
        assert_eq!(
            msg("p", "system.foo").as_deref(),
            Some("Invalid system namespace: p.system.foo")
        );
        assert_eq!(
            msg("p", "system.views").as_deref(),
            Some("cannot write to p.system.views")
        );
        assert_eq!(
            msg("p", "system.profile").as_deref(),
            Some("cannot write to p.system.profile")
        );
        assert_eq!(
            msg("p", "system.roles").as_deref(),
            Some("Invalid system namespace: p.system.roles")
        );
    }

    #[test]
    fn accepts_what_mongod_writes() {
        for coll in [
            "a b",
            "é",
            "a.",
            "a..b",
            "system.js",
            "system.users",
            "system.buckets.x",
        ] {
            assert_eq!(msg("p", coll), None, "{coll}");
        }
        assert_eq!(msg("admin", "system.roles"), None);
        assert_eq!(msg("admin", "system.version"), None);
    }

    #[test]
    fn the_namespace_limit_is_255() {
        assert_eq!(msg("probe", &"x".repeat(249)), None); // probe. + 249 = 255
        let long = "x".repeat(250);
        assert_eq!(
            msg("probe", &long),
            Some(format!(
                "Fully qualified namespace is too long. Namespace: probe.{long} Max: 255"
            ))
        );
    }
}

#[cfg(test)]
mod validator_problem_tests {
    //! A collection validator is parsed when it is set: `create` / `collMod`
    //! used to store one mongod refuses (measured 8.2.11, 2026-10-06).
    use super::validator_problem;
    use bson::doc;

    #[test]
    fn refuses_what_mongod_will_not_parse() {
        let p = |v| validator_problem(&v).map(|e| (e.code, e.errmsg));
        assert_eq!(
            p(doc! {"$jsonSchema": {"type": "integer"}}),
            Some((
                9,
                "$jsonSchema type 'integer' is not currently supported.".into()
            ))
        );
        assert_eq!(
            p(doc! {"$and": [{"$jsonSchema": {"nope": 1}}]}),
            Some((9, "Unknown $jsonSchema keyword: nope".into()))
        );
        assert_eq!(
            p(doc! {"a": {"$nope": 1}}),
            Some((2, "unknown operator: $nope".into()))
        );
        assert_eq!(
            p(doc! {"$jsonSchema": {"required": ["a"]}, "b": {"$gt": 1}}),
            None
        );
    }
}

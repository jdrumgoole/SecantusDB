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
//! * `create` unknown-field validation (`Location40415`); capped-size
//!   enforcement; `collMod`'s TTL-index `index: {expireAfterSeconds}` modify.
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
use crate::util::{
    as_i64, bool_field, coll_arg, collation_of, command_error, docs_to_bson, encode_docs,
};
use crate::{
    CommandContext, CommandError, HandlerResult, StorageError, DEFAULT_BATCH_SIZE, SERVER_VERSION,
};

/// Collection-option keys (from `create` / `collMod`) the Rust server persists.
/// `validator` + `validationLevel`/`validationAction` drive document validation;
/// `changeStreamPreAndPostImages` drives pre-image capture; `capped`/`size`/`max`
/// are reported in stats. (TTL-index `expireAfterSeconds` modification via
/// `collMod`'s `index` option is deferred.)
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
    if let Some(e) = validation_enum_problem(doc, "create") {
        return Ok(e.into_reply());
    }
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
            "Location40415",
            format!("BSON field 'create.{unknown}' is an unknown field"),
        )
        .into_reply());
    }
    // Build the options up front so they ride the `create` oplog entry (carried
    // by create_collection_with_options) — that's what lets PITR replay
    // reconstruct capped / validator / … rather than seeing a bare create.
    let mut opts = collection_option_subset(doc);
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
    Ok(doc! { "ok": 1.0 })
}

/// `collMod` — modify a collection's options (`validator` / `validationLevel` /
/// `validationAction` / `changeStreamPreAndPostImages`). Merges the recognised
/// options into the collection's stored blob. Errors `NamespaceNotFound` (26)
/// when the collection doesn't exist. (TTL-index `index` modification deferred.)
pub fn coll_mod(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
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
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns does not exist: {}.{}", ctx.db_name, coll),
        )
        .into_reply());
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
    let mut reply = doc! { "ok": 1.0 };
    // Index modification: `collMod {index: {keyPattern|name, prepareUnique|unique|expireAfterSeconds}}`.
    if let Some(Bson::Document(index_spec)) = doc.get("index") {
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
            // Both as int64, and no `_old` for an index that had no TTL.
            if let Some(old) = target.get("expireAfterSeconds").and_then(as_i64) {
                reply.insert("expireAfterSeconds_old", old);
            }
            reply.insert(
                "expireAfterSeconds_new",
                as_i64(new_expiry)
                    .map(Bson::Int64)
                    .unwrap_or(new_expiry.clone()),
            );
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"expireAfterSeconds": new_expiry.clone()},
                )
                .map_err(command_error)?;
        }
        // `prepareUnique` arms the index: new dup writes are rejected (11000)
        // while pre-existing duplicates are tolerated — the staging step before
        // a `unique: true` conversion.
        if let Some(prep) = index_spec.get("prepareUnique").and_then(Bson::as_bool) {
            storage
                .set_index_options(
                    &ctx.db_name,
                    &coll,
                    &target_name,
                    &doc! {"prepareUnique": prep},
                )
                .map_err(command_error)?;
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
                    format!("Cannot convert index {target_name} to unique: found duplicate values"),
                )
                .into_reply();
                reply.insert("violations", violations);
                return Ok(reply);
            }
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
        .coll_mod(&ctx.db_name, &coll, &opts)
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

/// `explain` — report the query plan (and, above `queryPlanner` verbosity,
/// execution counts) for a wrapped `find` / `aggregate` / `count` command. Ports
/// `commands.py::_explain`'s core: lifts a leading `$match` for aggregate, rejects
/// a journaled / `w:"majority"` writeConcern (72), validates `verbosity` (2),
/// shapes `queryPlanner.winningPlan` (`FETCH`+`IXSCAN` or `COLLSCAN`) and an
/// `executionStats` block (run via `find` to count). aggregate adds the
/// `stages: [{$cursor: …}, …]` wrapper drivers look for. Collation forces COLLSCAN.
pub fn explain(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let inner = doc
        .get("explain")
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    let cmd_name = inner.keys().next().cloned().unwrap_or_default();
    let coll = match inner.get(&cmd_name) {
        Some(Bson::String(s)) => s.clone(),
        _ => String::new(),
    };
    let mut filter = inner
        .get("filter")
        .or_else(|| inner.get("query"))
        .and_then(Bson::as_document)
        .cloned()
        .unwrap_or_default();
    let sort = inner.get("sort").and_then(Bson::as_document);
    let hint = inner.get("hint");
    // `find` / `findAndModify` read `sort: {$natural: ±1}` as storage order
    // (see `argtypes::natural_sort`), so the plan is the hinted collection
    // scan and not a blocking sort.
    let natural_hint = match sort {
        Some(s) if inner.contains_key("find") || inner.contains_key("findAndModify") => {
            crate::argtypes::natural_sort(s, hint)?
        }
        _ => None,
    };
    let sort = sort.filter(|_| natural_hint.is_none());
    let hint = natural_hint.as_ref().or(hint);
    let collation = collation_of(&inner);
    // Aggregate lifts a leading $match into the fetch — explain reports the same.
    if cmd_name == "aggregate" && filter.is_empty() {
        if let Some(Bson::Array(p)) = inner.get("pipeline") {
            if let Some(Bson::Document(first)) = p.first() {
                if let Some(Bson::Document(m)) = first.get("$match") {
                    filter = m.clone();
                }
            }
        }
    }
    let filter = normalize_or(filter);
    // explain + a journaled / majority writeConcern is ill-formed (InvalidOptions).
    for wc in [doc.get("writeConcern"), inner.get("writeConcern")] {
        if let Some(Bson::Document(wc)) = wc {
            let journaled = matches!(
                wc.get("j"),
                Some(Bson::Boolean(true)) | Some(Bson::Int32(1))
            );
            if journaled || wc.get_str("w").ok() == Some("majority") {
                return Ok(CommandError::new(
                    72,
                    "InvalidOptions",
                    "Command does not support writeConcern when used with explain",
                )
                .into_reply());
            }
        }
    }
    let verbosity = doc.get_str("verbosity").unwrap_or("executionStats");
    if !["queryPlanner", "executionStats", "allPlansExecution"].contains(&verbosity) {
        return Ok(CommandError::new(
            2,
            "BadValue",
            format!("verbosity {verbosity:?} not recognized"),
        )
        .into_reply());
    }

    let storage = ctx.storage()?;
    let ns = if coll.is_empty() {
        format!("{}.$cmd", ctx.db_name)
    } else {
        format!("{}.{}", ctx.db_name, coll)
    };
    // A collation (or no collection) forces COLLSCAN — the byte-sortable indexes
    // are collation-naive (mirrors `find`'s COLLSCAN-forcing under collation).
    let plan = if coll.is_empty() || collation.is_some() {
        let mut d = Document::new();
        d.insert("kind", "COLLSCAN");
        d
    } else {
        storage
            .explain_plan(&ctx.db_name, &coll, &filter, sort, hint)
            .map_err(command_error)?
    };
    let is_or = plan.get_str("kind").ok() == Some("OR");
    // An OR plan reads its documents through indexes too.
    let is_ixscan = plan.get_str("kind").ok() == Some("IXSCAN") || is_or;

    let (mut n_returned, mut docs_examined, mut keys_examined) = (0i64, 0i64, 0i64);
    if verbosity != "queryPlanner" && !coll.is_empty() {
        let res = storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &filter,
                sort,
                hint,
                collation.as_ref(),
                &Document::new(),
            )
            .map_err(command_error)?;
        n_returned = res.len() as i64;
        if is_ixscan {
            keys_examined = n_returned;
            docs_examined = n_returned;
        } else {
            docs_examined = storage
                .count_collated(&ctx.db_name, &coll, &Document::new(), None)
                .map_err(command_error)? as i64;
        }
    }

    let canonical = secantus_core::canonical_match(&Bson::Document(filter.clone()));
    // One IXSCAN node, with the index's own flags, in mongod's key order.
    let ixscan_node =
        |index_name: &str, key_pattern: &Document, multikey: bool, direction: &str| {
            let index_spec = if coll.is_empty() {
                Document::new()
            } else {
                storage
                    .list_indexes(&ctx.db_name, &coll)
                    .ok()
                    .and_then(|ixs| {
                        ixs.into_iter()
                            .find(|ix| ix.get_str("name").ok() == Some(index_name))
                    })
                    .unwrap_or_default()
            };
            let mut multikey_paths = Document::new();
            for field in key_pattern.keys() {
                multikey_paths.insert(
                    field.clone(),
                    Bson::Array(if multikey {
                        vec![Bson::String(field.clone())]
                    } else {
                        vec![]
                    }),
                );
            }
            doc! {
                "stage": "IXSCAN",
                "keyPattern": key_pattern.clone(),
                "indexName": index_name,
                "isMultiKey": multikey,
                "multiKeyPaths": multikey_paths,
                "isUnique": index_spec.get_bool("unique").unwrap_or(false),
                "isSparse": index_spec.get_bool("sparse").unwrap_or(false),
                "isPartial": index_spec.contains_key("partialFilterExpression"),
                "indexVersion": index_spec.get_i32("v").unwrap_or(2),
                "direction": direction,
            }
        };
    let winning_plan = if is_or {
        // mongod's OR plan: an IXSCAN per branch under OR, under a FETCH
        // carrying whatever the filter says beside the `$or`.
        let inputs: Vec<Bson> = plan
            .get_array("branches")
            .map(|bs| {
                bs.iter()
                    .filter_map(Bson::as_document)
                    .map(|b| {
                        Bson::Document(ixscan_node(
                            b.get_str("indexName").unwrap_or(""),
                            &b.get_document("keyPattern").cloned().unwrap_or_default(),
                            b.get_bool("multikey").unwrap_or(false),
                            b.get_str("direction").unwrap_or("forward"),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut rest = filter.clone();
        rest.remove("$or");
        let mut fetch = doc! { "stage": "FETCH" };
        if !rest.is_empty() {
            fetch.insert(
                "filter",
                Bson::Document(secantus_core::canonical_match(&Bson::Document(rest))),
            );
        }
        fetch.insert("inputStage", doc! { "stage": "OR", "inputStages": inputs });
        fetch
    } else if is_ixscan {
        let index_name = plan.get_str("indexName").unwrap_or("").to_string();
        let key_pattern = plan.get_document("keyPattern").cloned().unwrap_or_default();
        let multikey = plan.get_bool("multikey").unwrap_or(false);
        let input_stage = ixscan_node(
            &index_name,
            &key_pattern,
            multikey,
            plan.get_str("direction").unwrap_or("forward"),
        );
        // The FETCH stage carries only the RESIDUAL filter -- the predicate the
        // index bounds did not already satisfy -- and mongod OMITS the key
        // entirely when the bounds cover the whole filter. That is how a reader
        // tells a fully-index-served query from one that re-checks documents,
        // so echoing the whole filter here erased the distinction.
        let mut residual = Document::new();
        for (k, v) in filter.iter() {
            if !key_pattern.contains_key(k) {
                residual.insert(k.clone(), v.clone());
            }
        }
        let mut fetch = doc! { "stage": "FETCH" };
        if !residual.is_empty() {
            fetch.insert(
                "filter",
                Bson::Document(secantus_core::canonical_match(&Bson::Document(residual))),
            );
        }
        fetch.insert("inputStage", Bson::Document(input_stage));
        fetch
    } else {
        let mut collscan = doc! { "stage": "COLLSCAN" };
        if !canonical.is_empty() {
            collscan.insert("filter", Bson::Document(canonical.clone()));
        }
        // A `$natural: -1` hint is the only thing that walks the collection
        // backwards; every other collection scan is forward.
        let backward = match hint {
            Some(Bson::Document(d)) => match d.get("$natural") {
                Some(Bson::Int32(n)) => *n == -1,
                Some(Bson::Int64(n)) => *n == -1,
                Some(Bson::Double(n)) => *n == -1.0,
                _ => false,
            },
            _ => false,
        };
        collscan.insert("direction", if backward { "backward" } else { "forward" });
        collscan
    };
    // mongod wraps the scan in the stages that describe the rest of the query.
    // Only `find` is done here: `count` and `distinct` use a different
    // vocabulary (`COUNT` / `COUNT_SCAN` / `DISTINCT_SCAN`) that has not been
    // measured, and inventing stages for them would be worse than the flat node
    // they get today.
    let winning_plan = if cmd_name == "find" {
        let as_i64 = |v: Option<&Bson>| match v {
            Some(Bson::Int32(n)) => i64::from(*n),
            Some(Bson::Int64(n)) => *n,
            Some(Bson::Double(n)) => *n as i64,
            _ => 0,
        };
        secantus_core::build_stage_tree(
            winning_plan,
            sort,
            plan.get_bool("sortedByIndex").unwrap_or(false),
            inner.get("projection").and_then(Bson::as_document),
            as_i64(inner.get("skip")),
            as_i64(inner.get("limit")),
        )
    } else {
        winning_plan
    };
    // A filter that is ONLY an `$or` of two or more branches is planned per
    // branch: mongod wraps the whole tree in SUBPLAN, whatever is under it --
    // an index union or a collection scan (measured on 8.2.11, sort and
    // projection inside). A one-branch `$or` is its branch, and a filter with
    // anything beside the `$or` gets no SUBPLAN.
    let only_or = filter.len() == 1 && filter.get_array("$or").is_ok_and(|a| a.len() >= 2);
    let winning_plan = if only_or {
        doc! { "stage": "SUBPLAN", "inputStage": winning_plan }
    } else {
        winning_plan
    };
    // `isCached` sits on the OUTERMOST plan node only (the plan cache is a
    // whole-plan property) and is its FIRST key. We never cache plans.
    let winning_plan = {
        let mut outer = doc! { "isCached": false };
        for (k, v) in winning_plan.iter() {
            outer.insert(k.clone(), v.clone());
        }
        outer
    };
    let query_planner = doc! {
        "namespace": &ns,
        "indexFilterSet": false,
        // mongod echoes the NORMALISED match expression here, not the filter as
        // sent -- bare equality grows an explicit `$eq`, several fields become
        // a rank-sorted `$and`, `$ne` becomes `$not`/`$eq`, and so on. Echoing
        // the raw filter diverged on 44 of the 56 shapes in
        // `tools/probes/explain_shapes.py` while the Python server matched all
        // 56, because only it had this normalisation.
        "parsedQuery": Bson::Document(canonical.clone()),
        "winningPlan": winning_plan,
        "rejectedPlans": [],
    };
    let execution_stages = if is_ixscan {
        doc! {"stage": "FETCH", "nReturned": n_returned, "inputStage": {"stage": "IXSCAN", "nReturned": n_returned}}
    } else {
        doc! {"stage": "COLLSCAN", "nReturned": n_returned}
    };
    let mut exec_stats = doc! {
        "executionSuccess": true,
        "nReturned": n_returned,
        "executionTimeMillis": 0_i64,
        "totalKeysExamined": keys_examined,
        "totalDocsExamined": docs_examined,
        "executionStages": execution_stages,
    };
    // `allPlansExecution` verbosity adds per-candidate-plan stats under
    // executionStats. With a single solution (no multi-planning; rejectedPlans
    // is always empty) mongod emits an empty array — drivers' explain helpers
    // (mongo-php-library `ExplainFunctionalTest`) assert the key's presence.
    if verbosity == "allPlansExecution" {
        exec_stats.insert("allPlansExecution", Bson::Array(vec![]));
    }
    let server_info = doc! {
        "host": "secantus", "port": 0_i32, "version": SERVER_VERSION, "gitVersion": "0".repeat(40),
    };

    let mut reply = Document::new();
    if cmd_name == "aggregate" {
        let mut cursor = doc! { "queryPlanner": query_planner.clone() };
        if verbosity != "queryPlanner" {
            cursor.insert("executionStats", exec_stats.clone());
        }
        let mut stages = vec![Bson::Document(doc! { "$cursor": cursor })];
        if let Some(Bson::Array(p)) = inner.get("pipeline") {
            for s in p {
                stages.push(s.clone());
            }
        }
        reply.insert("stages", stages);
    }
    reply.insert("queryPlanner", query_planner);
    if verbosity != "queryPlanner" {
        reply.insert("executionStats", exec_stats);
    }
    reply.insert("command", inner);
    reply.insert("serverInfo", server_info);
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// `drop` — drop a collection.
pub fn drop(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "drop")?;
    let ns = format!("{}.{}", ctx.db_name, coll);
    let storage = ctx.storage()?;
    // Kill the collection's cursors BEFORE the storage drop, not after: the
    // drop emits an oplog entry that wakes any awaitData getMore parked on a
    // tailable cursor, and that getMore must observe the tombstone set here so
    // it reports "collection dropped" instead of re-polling a collection that
    // is already gone. Non-tailable cursors are removed outright, so a later
    // getMore is CursorNotFound (mongo-c-driver's error_document/getmore).
    // Mirrors commands.py::_drop.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&ns);
    }
    // mongod reports the index count the collection HAD, `_id_` included -- 3
    // for a collection with two secondary indexes, where this always said 1
    // (measured 8.2.11, 2026-09-30). A collection that does not exist lists
    // nothing, and that branch reports no count at all.
    let n_indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map(|ix| ix.len().max(1))
        .unwrap_or(1);
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
    if let Some(Bson::Document(c)) = doc.get("cursor") {
        argtypes::require_number(c, "batchSize", "listCollections.cursor.batchSize")?;
    }
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());
    let name_only = bool_field(doc, "nameOnly", false);
    let names = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?;
    let mut entries: Vec<Document> = Vec::with_capacity(names.len());
    for n in &names {
        let mut options = storage
            .get_collection_options(&ctx.db_name, n)
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
        if let Ok(uuid) = storage.collection_uuid(&ctx.db_name, n) {
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
        let is_clustered = options.contains_key("clusteredIndex");
        let mut entry = doc! {
            "name": n,
            "type": coll_type,
            "options": Bson::Document(options),
            "info": info,
        };
        // A clustered collection has no separate `_id_` index (the clustering
        // key IS the index), so mongod omits `idIndex` for it — same as views.
        if !is_view && !is_clustered {
            entry.insert(
                "idIndex",
                doc! {
                    "v": 2,
                    "key": { "_id": 1 },
                    "name": "_id_",
                    "ns": format!("{}.{}", ctx.db_name, n),
                },
            );
        }
        entries.push(entry);
    }
    // `filter` is evaluated against the full entry (so `{name: …}`,
    // `{type: …}`, `{"options.capped": true}` all work); apply it before the
    // `nameOnly` projection so a filter on `options` still matches.
    if let Some(f) = filter {
        entries.retain(|e| {
            secantus_core::query::matches(e, f, &Document::new(), None).unwrap_or(false)
        });
    }
    if name_only {
        for e in &mut entries {
            let name = e.get_str("name").unwrap_or("").to_string();
            let ty = e.get_str("type").unwrap_or("collection").to_string();
            *e = doc! { "name": name, "type": ty };
        }
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

/// `listDatabases` — descriptors for every database, honouring `filter` and
/// `nameOnly`. Mirrors `commands._list_databases`: each descriptor is
/// `{name, sizeOnDisk, empty}` (`sizeOnDisk` = summed BSON doc bytes across the
/// db's collections; `empty` = size 0), reduced to `{name}` under `nameOnly`.
/// The `filter` is a query predicate evaluated against each descriptor; it's
/// applied after `totalSize` is accumulated, matching mongod.
pub fn list_databases(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let name_only = bool_field(doc, "nameOnly", false);
    let filter = doc
        .get("filter")
        .and_then(Bson::as_document)
        .filter(|d| !d.is_empty());

    let names = storage.list_databases().map_err(command_error)?;
    let mut descriptors: Vec<Document> = Vec::new();
    let mut total_size: i64 = 0;
    for n in &names {
        if name_only {
            descriptors.push(doc! { "name": n });
            continue;
        }
        let mut size: i64 = 0;
        for coll in storage.list_collections(n).map_err(command_error)? {
            size += storage
                .collection_data_size(n, &coll)
                .map_err(command_error)?;
        }
        total_size += size;
        descriptors.push(doc! { "name": n, "sizeOnDisk": size, "empty": size == 0 });
    }
    if let Some(f) = filter {
        descriptors.retain(|d| {
            secantus_core::query::matches(d, f, &Document::new(), None).unwrap_or(false)
        });
    }
    Ok(doc! {
        "databases": Bson::Array(descriptors.into_iter().map(Bson::Document).collect()),
        "totalSize": total_size,
        "ok": 1.0,
    })
}

/// `listIndexes` — a cursor over the indexes of a collection.
pub fn list_indexes(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    // Nullable here, unlike aggregate's -- mongod accepts `cursor: null`.
    argtypes::require_cursor_object_nullable(doc)?;
    if let Some(Bson::Document(c)) = doc.get("cursor") {
        argtypes::require_number(c, "batchSize", "listIndexes.cursor.batchSize")?;
    }
    let coll = coll_arg(doc, "listIndexes")?;
    let storage = ctx.storage()?;
    let cursors = ctx.cursors()?;
    let mut indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    // `_id_` is always listed first, as on mongod. The rest come back in name
    // order here (mongod: creation order), which put an index named `$**_1`
    // or `A_1` ahead of it.
    indexes.sort_by_key(|ix| ix.get_str("name") != Ok("_id_"));
    // A collection that exists always has at least the synthesised `_id_` index, so
    // an empty result means the namespace doesn't exist — mongod errors
    // NamespaceNotFound (mongo-ruby-driver `Index::View#each ... collection does not
    // exist`). Mirrors commands.py's `if not indexes`.
    if indexes.is_empty() {
        return Ok(CommandError::new(
            26,
            "NamespaceNotFound",
            format!("ns does not exist: {}.{}", ctx.db_name, coll),
        )
        .into_reply());
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
    }
    // A clustered collection's clustering key IS its index: mongod reports a
    // single entry carrying `clustered: true` (with the user's name) in place of
    // the synthesised `_id_`. Mirrors commands.py.
    if let Some(ci) = storage
        .get_collection_options(&ctx.db_name, &coll)
        .ok()
        .and_then(|o| o.get_document("clusteredIndex").ok().cloned())
    {
        let clustered = doc! {
            "v": ci.get_i32("v").unwrap_or(2),
            "key": { "_id": 1i32 },
            "name": ci.get_str("name").unwrap_or("_id_").to_string(),
            "unique": true,
            "clustered": true,
        };
        let mut rest: Vec<Document> = indexes
            .into_iter()
            .filter(|ix| ix.get_str("name") != Ok("_id_"))
            .collect();
        indexes = std::iter::once(clustered).chain(rest.drain(..)).collect();
    }
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
    let mut any_created = false;
    let mut any_existed = false;
    for spec in &specs {
        let Bson::Document(s) = spec else { continue };
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
            return Ok(CommandError::new(
                67,
                "CannotCreateIndex",
                format!(
                    "Error in specification {spec_text} :: caused by :: Unknown index plugin \
                     '{plugin}'"
                ),
            )
            .into_reply());
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
                "Location40415",
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
                return Ok(reply);
            }
            Err(e) => return Err(command_error(e)),
        };
        any_created |= created;
        any_existed |= !created;
    }

    let after = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?
        .len();
    // mongod's reply, in its order: the two counts, whether the collection
    // was created (left out when nothing was built), the commit quorum a
    // replica-set member reports, and the no-op note.
    let all_existed = !any_created && !specs.is_empty();
    // A collection created here already has its `_id` index when counted.
    let before = if created_coll { before.max(1) } else { before };
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
    // TTL: one field, and a non-negative 32-bit number of seconds.
    if let Some(seconds) = spec.get("expireAfterSeconds") {
        let ttl = |why: &str| {
            CommandError::new(
                67,
                "CannotCreateIndex",
                format!(". Index spec: {text} :: caused by :: {why}"),
            )
        };
        if let Some(n) = as_i64(seconds) {
            if matches!(seconds, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)) {
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
        if !matches!(k.as_str(), "v" | "key" | "name" | "ns")
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
    argtypes::render_stage_value(&Bson::Document(out))
}

/// The options that make two index specs different indexes.
/// Catalog bookkeeping that is not part of an index's options (and that
/// `listIndexes` already hides from clients).
const INDEX_CATALOG_ONLY: &[&str] = &["entryFormat", "multikey"];

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
    let before = listed.len();
    // The `_id` index is never dropped, by name, by key or in a list. By key
    // it used to be: `{dropIndexes: c, index: {_id: 1}}` removed it.
    let id_index = || CommandError::new(72, "InvalidOptions", "cannot drop _id index").into_reply();
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
            let named = listed
                .iter()
                .find(|idx| idx.get_document("key").map(|k| k == key).unwrap_or(false))
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
    reply.insert("ok", 1.0);
    Ok(reply)
}

pub fn drop_database(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    storage.drop_database(&ctx.db_name).map_err(command_error)?;
    Ok(doc! { "dropped": ctx.db_name.clone(), "ok": 1.0 })
}

/// `renameCollection` — rename `renameCollection` (a full `db.coll` ns) to `to`.
pub fn rename_collection(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
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
    let (src_db, src_coll) = split_ns(&src);
    let (dst_db, dst_coll) = split_ns(&to);

    let storage = ctx.storage()?;
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
        return Ok(CommandError::new(code, name, m).into_reply());
    }
    // A rename invalidates cursors open on the source (and the dropped target),
    // same as a drop — a later getMore then fails with CursorNotFound. Mirrors
    // commands.py::_rename_collection.
    if let Ok(cursors) = ctx.cursors() {
        cursors.kill_namespace(&src);
        if drop_target {
            cursors.kill_namespace(&to);
        }
    }
    Ok(doc! { "ok": 1.0 })
}

/// `collStats` — per-collection size / count / index statistics.
pub fn coll_stats(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "collStats")?;
    let storage = ctx.storage()?;
    let count = storage
        .count_matching(&ctx.db_name, &coll, &Document::new())
        .map_err(command_error)? as i64;
    let size = storage
        .collection_data_size(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let index_sizes = storage
        .index_sizes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let capped = storage
        .collection_is_capped(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let total_index_size: i64 = index_sizes.values().filter_map(as_i64).sum();
    let avg_obj_size = if count > 0 { size / count } else { 0 };
    let mut reply = doc! {
        "ns": format!("{}.{}", ctx.db_name, coll),
        "count": count as i32,
        "size": size,
        "avgObjSize": avg_obj_size,
        "storageSize": size,
        "nindexes": index_sizes.len() as i32,
        "totalIndexSize": total_index_size,
        "indexSizes": index_sizes,
        "capped": capped,
    };
    if capped {
        // mongod reports a capped collection's bounds here: `max` (0 when no
        // document limit was given) and the byte bound as `maxSize`.
        let opts = storage
            .get_collection_options(&ctx.db_name, &coll)
            .unwrap_or_default();
        reply.insert(
            "max",
            int_bson(opts.get("max").and_then(as_i64).unwrap_or(0)),
        );
        if let Some(size) = opts.get("size").and_then(as_i64) {
            reply.insert("maxSize", int_bson(size));
        }
    }
    reply.insert("ok", 1.0);
    Ok(reply)
}

/// `dbStats` — database-wide totals aggregated across collections.
pub fn db_stats(_doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let storage = ctx.storage()?;
    let colls = storage
        .list_collections(&ctx.db_name)
        .map_err(command_error)?;
    let mut objects = 0i64;
    let mut data_size = 0i64;
    let mut indexes = 0i64;
    let mut index_size = 0i64;
    for c in &colls {
        objects += storage
            .count_matching(&ctx.db_name, c, &Document::new())
            .map_err(command_error)? as i64;
        data_size += storage
            .collection_data_size(&ctx.db_name, c)
            .map_err(command_error)?;
        let isz = storage
            .index_sizes(&ctx.db_name, c)
            .map_err(command_error)?;
        indexes += isz.len() as i64;
        index_size += isz.values().filter_map(as_i64).sum::<i64>();
    }
    Ok(doc! {
        "db": ctx.db_name.clone(),
        "collections": colls.len() as i32,
        "objects": objects,
        "dataSize": data_size,
        "storageSize": data_size,
        "indexes": indexes,
        "indexSize": index_size,
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
    Ok(doc! {
        "host": "secantus",
        "version": crate::SERVER_VERSION,
        "process": "mongod",
        "pid": Bson::Int64(0),
        "uptime": 0.0,
        "uptimeMillis": Bson::Int64(0),
        "localTime": bson::DateTime::now(),
        "metrics": {
            "cursor": {
                "open": { "total": open_cursors, "pinned": 0i64, "noTimeout": 0i64 },
            },
        },
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
        "opcounters": {
            "insert": 0i32, "query": 0i32, "update": 0i32,
            "delete": 0i32, "getmore": 0i32, "command": 0i32,
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
        "network": { "numRequests": 0i32, "bytesIn": 0i32, "bytesOut": 0i32 },
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

    Ok(doc! {
        "inprog": vec![Bson::Document(op)],
        "ok": 1.0,
    })
}

/// `validate` — mongod's collection consistency check. SecantusDB stores
/// documents as opaque BSON and maintains index entries transactionally, so
/// there's nothing to repair: report a clean, mongod-shaped result with real
/// record / index counts. `full` / `background` / `scandata` are accepted and
/// ignored (they only affect how mongod scans, not the verdict). Ports
/// `commands.py::_validate`.
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
    if !storage
        .collection_exists(&ctx.db_name, &coll)
        .map_err(command_error)?
    {
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
    let nrecords = storage
        .count_matching(&ctx.db_name, &coll, &Document::new())
        .map_err(command_error)? as i64;
    let indexes = storage
        .list_indexes(&ctx.db_name, &coll)
        .map_err(command_error)?;
    let mut keys_per_index = Document::new();
    let mut index_details = Document::new();
    let mut n_indexes = 0i32;
    for ix in &indexes {
        if let Ok(name) = ix.get_str("name") {
            keys_per_index.insert(name, nrecords);
            index_details.insert(name, doc! { "valid": true });
            n_indexes += 1;
        }
    }
    Ok(doc! {
        "ns": format!("{}.{}", ctx.db_name, coll),
        "nInvalidDocuments": 0i64,
        "nNonCompliantDocuments": 0i64,
        "nrecords": nrecords,
        "nIndexes": n_indexes,
        "keysPerIndex": keys_per_index,
        "indexDetails": index_details,
        "valid": true,
        "repaired": false,
        "warnings": Bson::Array(vec![]),
        "errors": Bson::Array(vec![]),
        "extraIndexEntries": Bson::Array(vec![]),
        "missingIndexEntries": Bson::Array(vec![]),
        "corruptRecords": Bson::Array(vec![]),
        "ok": 1.0,
    })
}

/// `profile` — get / set per-database profiling level. `{profile: -1}` reads;
/// `{profile: 0|1|2, slowms, sampleRate}` updates. The reply carries the
/// PREVIOUS values under `was` / `slowms` / `sampleRate`. Ports
/// `commands.py::_profile`. (SecantusDB records the level but does no actual
/// slow-op profiling — `system.profile` stays a faithful stub.)
pub fn profile(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let db = ctx.db_name.clone();
    let storage = ctx.storage()?;
    let prev = storage.get_profile(&db).map_err(command_error)?;
    let prev_level = prev.get_i32("level").unwrap_or(0);
    // `slowms` and `sampleRate` are server-wide on mongod: set through one
    // database, read back through every other (probed on 8.2.11). Only the
    // level is per-database.
    let slow_ops = ctx.slow_ops.clone();
    let (prev_slowms, prev_rate) = match &slow_ops {
        Some(s) => (s.slow_ms(), s.sample_rate()),
        None => (
            prev.get_i32("slowms").unwrap_or(100),
            prev.get_f64("sampleRate").unwrap_or(1.0),
        ),
    };
    let was = doc! {
        "was": prev_level,
        "slowms": prev_slowms,
        "sampleRate": prev_rate,
        "ok": 1.0,
    };
    let arg = match doc.get("profile") {
        Some(Bson::Int32(n)) => Some(*n),
        Some(Bson::Int64(n)) => Some(*n as i32),
        Some(Bson::Double(d)) if d.fract() == 0.0 => Some(*d as i32),
        _ => None,
    };
    match arg {
        Some(-1) => Ok(was),
        Some(level) if (0..=2).contains(&level) => {
            let slowms = doc
                .get("slowms")
                .and_then(as_i64)
                .map(|n| n as i32)
                .unwrap_or(prev_slowms);
            let rate = doc
                .get("sampleRate")
                .and_then(Bson::as_f64)
                .unwrap_or(prev_rate);
            storage
                .set_profile(&db, level, slowms, rate)
                .map_err(command_error)?;
            if let Some(s) = &slow_ops {
                s.set(slowms, rate);
            }
            Ok(was)
        }
        _ => Ok(
            CommandError::new(14, "TypeMismatch", "profile must be -1, 0, 1, or 2").into_reply(),
        ),
    }
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

/// mongod's normalisation of a filter that is ONLY an `$or`, before planning
/// (measured on 8.2.11): a one-branch `$or` is its branch, and an `$or` of
/// plain equalities on ONE field is that field's `$in` -- so neither gets the
/// OR plan's SUBPLAN.
fn normalize_or(filter: Document) -> Document {
    let Some(Bson::Array(arms)) = filter.get("$or").filter(|_| filter.len() == 1) else {
        return filter;
    };
    if let [Bson::Document(only)] = arms.as_slice() {
        return only.clone();
    }
    let mut field: Option<&str> = None;
    let mut values = Vec::new();
    for arm in arms {
        let Bson::Document(d) = arm else {
            return filter.clone();
        };
        let [(k, v)] = d.iter().collect::<Vec<_>>()[..] else {
            return filter.clone();
        };
        if k.starts_with('$') || field.is_some_and(|f| f != k) {
            return filter.clone();
        }
        let value = match v {
            Bson::Document(op) if op.keys().any(|key| key.starts_with('$')) => {
                match (op.len(), op.get("$eq")) {
                    (1, Some(eq)) => eq.clone(),
                    _ => return filter.clone(),
                }
            }
            Bson::RegularExpression(_) | Bson::Array(_) => return filter.clone(),
            other => other.clone(),
        };
        field = Some(k);
        values.push(value);
    }
    match field {
        Some(f) => doc! { f: { "$in": values } },
        None => filter.clone(),
    }
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
        assert_eq!((code, name.as_str()), (40415, "Location40415"));
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
        let reply = list_indexes(
            &doc! {"listIndexes": "specs", "cursor": {"batchSize": -100_i32}},
            &mut c,
        )
        .unwrap();
        let (code, name, msg) = err_of(&reply);
        assert_eq!((code, name.as_str()), (2, "BadValue"));
        assert!(msg.contains("must be >= 0"), "{msg}");
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

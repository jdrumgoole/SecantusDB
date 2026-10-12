//! The `distinct` command — distinct values of a field across matching docs.
//!
//! Port of `commands.py::_distinct`: fetch the matching documents, resolve the
//! dotted `key` path on each (flattening one array level, as mongod does), and
//! collect the distinct values. A command `collation` applies to both the query
//! filter (collation-aware COLLSCAN) and the value dedup (collation-equal strings
//! collapse to one).

use bson::{doc, Bson, Document};

use crate::argtypes;
use crate::util::{coll_arg, command_error, doc_field};
use crate::{CommandContext, CommandError, HandlerResult};

/// `distinct` — return the distinct values of `key` over docs matching `query`.
pub fn distinct(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "distinct")?;
    const FIELDS: &[&str] = &[
        "distinct",
        "key",
        "query",
        "collation",
        "hint",
        "mirrored",
        "sampleId",
        "rawData",
        "includeQueryStatsMetrics",
        "querySettings",
    ];
    if let Some(key) = argtypes::unknown_field(doc, FIELDS) {
        return Err(CommandError::new(
            40415,
            "IDLUnknownField",
            format!("BSON field 'distinctCommandRequest.{key}' is an unknown field."),
        ));
    }
    // mongod 8.2 reports every field under the IDL struct name
    // (`distinctCommandRequest`), `key` included -- an older probe had `key` as
    // `distinct.key`; re-measured 8.2.11, 2026-10-01.
    argtypes::require_object(doc, "query", "distinctCommandRequest.query")?;
    argtypes::require_object(doc, "collation", "distinctCommandRequest.collation")?;
    // An undefined `$$variable` is a PARSE error (17276), not the storage
    // layer's generic "unsupported construct" BadValue.
    {
        let bound: Vec<String> = match doc.get("let") {
            Some(Bson::Document(d)) => d.keys().cloned().collect(),
            _ => Vec::new(),
        };
        if let Some(Bson::Document(f)) = doc.get("query") {
            if let Some((code, msg)) = argtypes::expression_problem_in_filter(f, &bound) {
                return Err(CommandError::new(
                    code,
                    crate::util::error_code_name(code),
                    msg,
                ));
            }
        }
    }

    let key = match doc.get("key") {
        Some(Bson::String(s)) => s.clone(),
        // An explicit null is MISSING to mongod, not wrong-typed: a required
        // field that is absent answers 40414, not a TypeMismatch. Probed.
        None | Some(Bson::Null) => {
            return Ok(CommandError::new(
                40414,
                "IDLFailedToParse",
                "BSON field 'distinctCommandRequest.key' is missing but a required field",
            )
            .into_reply())
        }
        Some(v) => {
            return Ok(CommandError::new(
                14,
                "TypeMismatch",
                format!(
                    "BSON field 'distinctCommandRequest.key' is the wrong type '{}', expected type 'string'",
                    secantus_core::query::bson_type_name(v)
                ),
            )
            .into_reply())
        }
    };
    if key.contains('\0') {
        return Err(CommandError::new(
            31032,
            "Location31032",
            "Key field cannot contain an embedded null byte",
        ));
    }
    let filter = doc_field(doc, "query");
    let collation = crate::util::effective_collation(ctx.storage()?, &ctx.db_name, &coll, doc)?;
    let _active = secantus_core::collation::activate(collation.as_ref());
    // `distinct` takes a hint like every other read, and mongod REFUSES the
    // command when it names no index rather than scanning (probed 8.2.11,
    // 2026-08-31: code 2; a valid index name or key spec is accepted). This
    // handler passed `None` here, so a bogus hint silently returned full
    // results -- the only hint-bearing command that did not resolve the field.
    // Passing it validates AND honours the hint, including a sparse index's
    // reduced document set, the way `find` does.
    argtypes::require_hint(doc, "hint")?;
    let hint = argtypes::given_hint(doc);
    argtypes::require_natural_hint(hint)?;
    let storage = ctx.storage()?;
    // A time-series collection is a view over its buckets on mongod, and
    // answers as one.
    let on_view = crate::views::is_view(storage, &ctx.db_name, &coll);
    let as_view = on_view
        || storage
            .get_collection_options(&ctx.db_name, &coll)
            .is_ok_and(|o| o.contains_key("timeseries"));
    // A view has no rows of its own: its documents are its pipeline's output
    // over the collection it is defined on. Reading the view's own (empty)
    // storage answered `[]` for every `distinct` on a view.
    let documents: Vec<Document> = if on_view {
        crate::aggregate::collection_documents(
            storage,
            &ctx.db_name,
            &coll,
            &filter,
            collation.as_ref(),
        )?
    } else {
        let bytes = storage
            .find_collated(
                &ctx.db_name,
                &coll,
                &filter,
                None,
                hint,
                collation.as_ref(),
                &Document::new(),
            )
            .map_err(command_error)?;
        let mut decoded = Vec::with_capacity(bytes.len());
        for b in bytes {
            decoded.push(
                secantus_core::bsondoc::from_bytes(b.as_slice())
                    .map_err(|e| CommandError::new(1, "InternalError", format!("decode: {e}")))?,
            );
        }
        decoded
    };

    // Every value the key reaches, through arrays of documents on the way and
    // one level into an array at the end.
    let mut values: Vec<Bson> = Vec::new();
    for d in &documents {
        let (found, _) = secantus_core::get_path_values(d, &key);
        for v in found {
            match v {
                Bson::Array(arr) => values.extend(arr.iter().cloned()),
                other => values.push(other.clone()),
            }
        }
    }
    // mongod answers the distinct values in BSON sort order, and "distinct"
    // is BSON equality: 1, 1.0 and a Decimal128 1.0 are one value, as are 0
    // and -0.0, and two strings the collation holds equal. The first one met
    // is the one returned (the sort is stable). Comparing with `==` kept one
    // of each TYPE.
    values.sort_by(secantus_core::order::cmp);
    values.dedup_by(|later, first| {
        secantus_core::order::cmp(later, first) == std::cmp::Ordering::Equal
    });
    if as_view {
        // mongod runs a view's `distinct` as an aggregation and answers
        // `ok` as an int32 there, where a collection's is a double.
        return Ok(doc! { "values": values, "ok": 1_i32 });
    }
    Ok(doc! { "values": values, "ok": 1.0 })
}

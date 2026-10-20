//! The `distinct` command — distinct values of a field across matching docs.
//!
//! Port of `commands.py::_distinct`: fetch the matching documents, resolve the
//! dotted `key` path on each (flattening one array level, as mongod does), and
//! collect the distinct values. A command `collation` applies to both the query
//! filter (collation-aware COLLSCAN) and the value dedup (collation-equal strings
//! collapse to one).

use bson::{doc, Bson, Document};

use crate::argtypes;
use crate::util::{coll_arg, doc_field};
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
    if let Some(problem) = crate::crud::read_option_problem(doc, "distinctCommandRequest") {
        return Err(problem);
    }
    // mongod will not read a collection of a name it could not create
    // (measured 8.2.11, 2026-10-12); these answered an empty list.
    if coll.starts_with('.') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Collection names cannot start with '.': {coll}"),
        ));
    }
    if coll.contains('$') {
        return Err(CommandError::new(
            73,
            "InvalidNamespace",
            format!("Invalid collection name specified '{}.{coll}'", ctx.db_name),
        ));
    }
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
    if let Some((code, msg)) =
        crate::crud::text_query_problem(ctx.storage()?, &ctx.db_name, &coll, &filter)
    {
        return Err(CommandError::new(
            code,
            crate::util::error_code_name(code),
            msg,
        ));
    }
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
    crate::timeseries::check_hint(storage, &ctx.db_name, &coll, &filter, hint)?;
    let on_view = crate::views::is_view(storage, &ctx.db_name, &coll);
    let as_view = on_view
        || storage
            .get_collection_options(&ctx.db_name, &coll)
            .is_ok_and(|o| o.contains_key("timeseries"));
    // A view has no rows of its own: its documents are its pipeline's output
    // over the collection it is defined on. Reading the view's own (empty)
    // storage answered `[]` for every `distinct` on a view.
    let mut values: Vec<Bson> = if on_view {
        crate::views::check_hint(storage, &ctx.db_name, &coll, hint, &filter)?;
        if crate::notablescan::enabled(ctx) {
            // The view's pipeline, then the query: what mongod plans.
            let tail = if filter.is_empty() {
                Vec::new()
            } else {
                vec![Bson::Document(bson::doc! { "$match": filter.clone() })]
            };
            let (base, pipeline) =
                crate::aggregate::resolve_view(storage, &ctx.db_name, &coll, tail)?;
            let (query, sort) = crate::explain::pipeline_query(&base, &pipeline);
            crate::notablescan::check(
                ctx,
                &crate::notablescan::Read {
                    coll: &base,
                    filter: &query,
                    sort: sort.as_ref(),
                    hint: None,
                    collation: collation.as_ref(),
                    shown: Default::default(),
                },
            )?;
        }
        document_values(
            &crate::aggregate::collection_documents(
                storage,
                &ctx.db_name,
                &coll,
                &filter,
                collation.as_ref(),
            )?,
            &key,
        )
    } else {
        let read = crate::notablescan::Read {
            coll: &coll,
            filter: &filter,
            sort: None,
            hint,
            collation: collation.as_ref(),
            shown: crate::notablescan::Shown {
                collation: doc.get_document("collation").ok().cloned(),
                ..Default::default()
            },
        };
        read.check_hint(storage, &ctx.db_name)?;
        crate::notablescan::check(ctx, &read)?;
        let ask = crate::explain::order::Ask {
            db: &ctx.db_name,
            coll: &coll,
            filter: &filter,
            sort: None,
            hint,
            collation: collation.as_ref(),
            vars: &Document::new(),
            limit: 0,
            asked: Default::default(),
        };
        let read_error = |e| {
            crate::util::read_error_on_namespace(e, "distinct", &format!("{}.{coll}", ctx.db_name))
        };
        // A plan that is a DISTINCT_SCAN answers from the index's keys.
        let scanned =
            crate::explain::distinct::scan_values(storage, &ask, &key).map_err(read_error)?;
        let values = match scanned {
            Some(values) => values,
            None => {
                let bytes = crate::explain::order::find(storage, &ask).map_err(read_error)?;
                let mut decoded = Vec::with_capacity(bytes.len());
                for b in bytes {
                    decoded.push(secantus_core::bsondoc::from_bytes(b.as_slice()).map_err(
                        |e| CommandError::new(1, "InternalError", format!("decode: {e}")),
                    )?);
                }
                document_values(&decoded, &key)
            }
        };
        crate::explain::order::count_index_use(
            storage,
            &ask,
            crate::explain::order::Uses::Distinct(&key),
        );
        values
    };
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

/// Every value `key` reaches in `documents`, through arrays of documents on
/// the way and one level into an array at the end.
fn document_values(documents: &[Document], key: &str) -> Vec<Bson> {
    let mut values: Vec<Bson> = Vec::new();
    for d in documents {
        let (found, _) = secantus_core::get_path_values(d, key);
        for v in found {
            match v {
                Bson::Array(arr) => values.extend(arr.iter().cloned()),
                other => values.push(other.clone()),
            }
        }
    }
    values
}

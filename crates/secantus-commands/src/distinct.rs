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

/// The fields mongod's `distinct` takes beside the generic arguments.
pub(crate) const DISTINCT_FIELDS: &[&str] = &[
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

/// `distinct` — return the distinct values of `key` over docs matching `query`.
pub fn distinct(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let coll = coll_arg(doc, "distinct")?;
    const FIELDS: &[&str] = DISTINCT_FIELDS;
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
    crate::opdebug::note_resolved_views(storage, &ctx.db_name, Some(&coll), &[]);
    let on_view = crate::views::is_view(storage, &ctx.db_name, &coll);
    // Under a query setting's `queryFramework: "sbe"` mongod runs the
    // query in SBE, which decides how its expressions answer (RM-542).
    let sbe = !on_view
        && crate::explain::find_runs_in_sbe(
            storage,
            &ctx.db_name,
            &coll,
            &filter,
            None,
            0,
            hint,
            collation.as_ref(),
        );
    let _engine = secantus_core::sbe::enter(sbe);
    let as_view = on_view
        || storage
            .get_collection_options(&ctx.db_name, &coll)
            .is_ok_and(|o| o.contains_key("timeseries"));
    // A view has no rows of its own: its documents are its pipeline's output
    // over the collection it is defined on. Reading the view's own (empty)
    // storage answered `[]` for every `distinct` on a view.
    // What the plan examined as it read (RM-023).
    let mut examined: Option<(i64, i64)> = None;
    let mut stages: Option<Document> = None;
    let mut values: Vec<Bson> = if on_view {
        crate::views::check_hint(storage, &ctx.db_name, &coll, hint, &filter)?;
        match view_values(ctx, &coll, &key, &filter, hint, collation.as_ref())? {
            Ok(values) => values,
            Err(refused) => return Ok(refused),
        }
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
            asked: crate::near::Asked {
                cached: true,
                distinct: Some(&key),
                ..Default::default()
            },
        };
        let read_error = |e| {
            crate::util::read_error_on_namespace(e, "distinct", &format!("{}.{coll}", ctx.db_name))
        };
        // A plan that is a DISTINCT_SCAN answers from the index's keys.
        let streamed = streamed_values(storage, &ask, &key).map_err(read_error)?;
        let scanned = match streamed {
            Some((values, keys, docs, run)) => {
                examined = Some((keys, docs));
                stages = run;
                Some(values)
            }
            None => {
                crate::explain::distinct::scan_values(storage, &ask, &key).map_err(read_error)?
            }
        };
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
    if let Some((keys_examined, docs_examined)) = examined {
        ctx.exec_stats = Some(crate::ExecStats {
            keys_examined,
            docs_examined,
            exec_stages: stages,
            ..Default::default()
        });
    }
    Ok(doc! { "values": values, "ok": 1.0 })
}

/// A `distinct` of a view, run as mongod 8.2.11 runs it: the aggregation
/// over the view that `ParsedDistinctCommand::asAggregationCommand` builds
/// (the query's `$match`, then every value at `key` as one document each,
/// gathered by a `$group`; its `explain` prints it, measured 2026-11-13),
/// under the view's collation. Its plan is the one the line names, and what
/// that plan examined and its exhausted cursor are the command's (RM-493).
/// `Err`: the aggregation's refusal, as its reply.
fn view_values(
    ctx: &mut CommandContext,
    view: &str,
    key: &str,
    filter: &Document,
    hint: Option<&Bson>,
    collation: Option<&secantus_core::collation::Collation>,
) -> Result<Result<Vec<Bson>, Document>, CommandError> {
    let mut pipeline: Vec<Bson> = Vec::new();
    if !filter.is_empty() {
        pipeline.push(Bson::Document(doc! { "$match": filter.clone() }));
    }
    pipeline.extend([
        Bson::Document(doc! { "$replaceRoot": { "newRoot": {
            "_internalUnwoundArray": { "$_internalFindAllValuesAtPath": key }
        } } }),
        Bson::Document(doc! { "$unwind": {
            "path": "$_internalUnwoundArray",
            "preserveNullAndEmptyArrays": true,
        } }),
        Bson::Document(doc! { "$group": {
            "_id": Bson::Null,
            "distinct": { "$addToSet": "$_internalUnwoundArray" },
        } }),
    ]);
    let mut agg = doc! {
        "aggregate": view,
        "pipeline": pipeline,
        "cursor": {},
        "collation": match collation {
            Some(c) => c.spec().clone(),
            None => doc! { "locale": "simple" },
        },
    };
    if let Some(hint) = hint {
        agg.insert("hint", hint.clone());
    }
    let reply = crate::aggregate::aggregate_inline(&agg, ctx)?;
    if !crate::util::reply_ok(&reply) {
        return Ok(Err(reply));
    }
    let cursor = reply.get_document("cursor").ok();
    let values = cursor
        .and_then(|c| c.get_array("firstBatch").ok())
        .and_then(|b| b.first())
        .and_then(Bson::as_document)
        .and_then(|d| d.get_array("distinct").ok())
        .cloned()
        .unwrap_or_default();
    // A `$group` answers in one batch: the cursor is exhausted, and the
    // read's counts are the command's, not a cursor's.
    let mut stats = ctx
        .exec_stats
        .take()
        .or_else(crate::opdebug::take_examined)
        .unwrap_or_default();
    stats.exhausted = cursor.and_then(|c| c.get_i64("id").ok()).unwrap_or(0) == 0;
    stats.cursor_id = 0;
    ctx.exec_stats = Some(stats);
    Ok(Ok(values))
}

/// The values of `key`, read as mongod's plan for the `distinct` reads them
/// (RM-023), with the keys and documents it examined: a DISTINCT_SCAN's
/// from its keys (or, through a collated index, from the first document of
/// each), any other plan's from the documents it returns. `None`: the plan
/// is not one a stream runs, and the values are read as before.
#[allow(clippy::type_complexity)]
fn streamed_values(
    storage: &dyn crate::Storage,
    ask: &crate::explain::order::Ask,
    key: &str,
) -> Result<Option<(Vec<Bson>, i64, i64, Option<Document>)>, crate::storage::StorageError> {
    use crate::explain::Node;
    let planned = crate::explain::order::Planned::Distinct(key);
    let Some(plan) = crate::explain::order::counted_plan(storage, ask, planned)? else {
        return Ok(None);
    };
    let scan = match &plan {
        Node::Projection { input, .. } => input.as_ref(),
        other => other,
    };
    // A covered DISTINCT_SCAN answers from its keys.
    let from_keys = matches!(scan, Node::DistinctScan(_, _, false));
    let opts = crate::explain::stream::Options {
        keys: from_keys,
        collation: ask.collation,
        ..Default::default()
    };
    let Some(mut stream) = crate::explain::stream::Stream::of_plan_with(
        &plan, ask.db, ask.coll, ask.filter, ask.vars, &opts,
    )?
    else {
        return Ok(None);
    };
    let all = stream.read_all(storage)?;
    // Its stages, as its profile entry gives them (RM-359).
    let stages = crate::profiler::profiled()
        .then(|| stream.exec_stats(&plan))
        .flatten();
    let mut values = Vec::new();
    if from_keys {
        for k in all.keys {
            match k.and_then(|d| d.get(key).cloned()).unwrap_or(Bson::Null) {
                Bson::Array(items) => values.extend(items),
                other => values.push(other),
            }
        }
    } else {
        let mut decoded = Vec::with_capacity(all.docs.len());
        for b in &all.docs {
            decoded.push(
                secantus_core::bsondoc::from_bytes(b.as_slice())
                    .map_err(|e| crate::storage::StorageError::Internal(format!("decode: {e}")))?,
            );
        }
        values = document_values(&decoded, key);
    }
    Ok(Some((values, all.keys_examined, all.docs_examined, stages)))
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

//! An `aggregate` cursor that reads on as `getMore` asks (RM-080).
//!
//! mongod reads an aggregate's collection one of two ways (measured 8.2.11,
//! 2026-10-24). A pipeline its query planner takes whole, or whose stages it
//! pushes into SBE with the read, is a plan executor read batch by batch, as
//! a `find` is. Any other reads through its cursor stage, which buffers the
//! query's documents: 32 at first, twice as many at each refill, and no more
//! once a buffer holds 4 MB. The stages after it run on each buffered
//! document only as the cursor needs it, so an error in a later document is
//! the `getMore`'s. A `$unionWith` passes the collection's documents on,
//! then opens its own collection when the cursor reaches it and reads that
//! through a cursor stage of its own. A `$group` mongod answers from a
//! DISTINCT_SCAN (`$groupByDistinctScan`) reads the scan the same way, its
//! groups in the scan's order. A pipeline with a stage that blocks
//! (`$group`, an unindexed `$sort`) reads everything for its first batch,
//! and is not streamed here.

use std::collections::VecDeque;

use bson::{doc, Bson, Document};

use crate::cursors::{StreamCursor, StreamedBatch};
use crate::explain::stream::Stream;
use crate::storage::Storage;
use crate::{CommandContext, CommandError, MAX_GETMORE_BATCH_BYTES};

/// The cursor stage's first buffer, in documents; each refill doubles it.
const FIRST_BUFFER: usize = 32;
/// The bytes a cursor-stage buffer stops at.
const BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// The stages that pass each document on as it arrives. A pipeline of
/// anything else is read whole.
const STREAMING_STAGES: &[&str] = &[
    "$match",
    "$project",
    "$unset",
    "$addFields",
    "$set",
    "$replaceRoot",
    "$replaceWith",
    "$unwind",
    "$redact",
    "$skip",
    "$limit",
    "$lookup",
    "$unionWith",
];

/// The stages after a cursor stage whose `getNext` calls -- each one tick
/// of mongod's `ExpressionContext` interrupt checker -- are counted here
/// (RM-360): each passes on one document per call, or (`$skip`) drops some,
/// or (`$unwind`) makes several of one. A pipeline with any other prints no
/// count: a `$limit` there stops its source short of the documents this
/// cursor runs in one go, a `$lookup` or `$unionWith` reads through
/// pipelines of its own, and a `$match` or `$redact` left after the read
/// is not always where mongod's optimizer leaves it (RM-538: a `$match` on
/// a field an `$addFields` renames moves ahead of it onto the field).
const COUNTED_STAGES: &[&str] = &[
    "$project",
    "$unset",
    "$addFields",
    "$set",
    "$replaceRoot",
    "$replaceWith",
    "$unwind",
    "$skip",
];

/// Whether a stage reads metadata this cursor rewrites into stages of its
/// own (`agg_meta`), which mongod's pipeline does not have.
fn metas_or_sort_keys(stages: &[Bson]) -> bool {
    stages
        .iter()
        .any(|s| mentions_meta(s) || crate::agg_meta::reads_sort_key(s))
}

/// What an `aggregate` over a collection asks to read.
pub(crate) struct Ask<'a> {
    pub db: &'a str,
    pub coll: &'a str,
    pub ns: &'a str,
    pub pipeline: &'a [Bson],
    pub hint: Option<&'a Bson>,
    pub vars: &'a Document,
    pub batch_size: i64,
    pub command: &'a Document,
    /// The command's collation (a view's own, through a view).
    pub collation: Option<&'a secantus_core::collation::Collation>,
    /// The cursor names the collection it reads, not a view over it.
    pub direct: bool,
}

/// An opened cursor: the reply, and the first batch that goes in it.
pub(crate) struct Opened {
    pub reply: Document,
    pub batch: Vec<Vec<u8>>,
    /// What reading the first batch examined, for the command's line.
    pub stats: crate::ExecStats,
}

/// Open `ask`'s cursor, when its pipeline streams. `None`: read it whole.
pub(crate) fn open(ctx: &CommandContext, ask: &Ask) -> Result<Option<Opened>, CommandError> {
    let storage = ctx
        .storage
        .clone()
        .ok_or_else(|| CommandError::new(1, "InternalError", "no storage configured"))?;
    let storage = storage.as_ref();
    let Some((mut cursor, indexes)) = build(storage, ask, false)? else {
        return Ok(None);
    };
    cursor.summary = crate::explain::summary::plan_summary(ask.command, ctx);
    cursor.budgets = crate::aggregate::budgets_of(ctx);
    // Under `notablescan` the read itself was checked when the command
    // planned it; the stages that read other collections are checked as
    // each batch runs them, as the aggregate's own run checks them.
    cursor.notablescan = crate::notablescan::enabled(ctx).then(|| ask.ns.to_string());
    // `batchSize: 0` asks for the cursor and reads nothing.
    let batch = if ask.batch_size == 0 {
        StreamedBatch {
            docs: Vec::new(),
            exhausted: false,
            keys_examined: 0,
            docs_examined: 0,
        }
    } else {
        cursor.next_batch(
            storage,
            ask.batch_size.max(0) as usize,
            MAX_GETMORE_BATCH_BYTES,
        )?
    };
    let cursor_id = if batch.exhausted {
        0
    } else {
        // A cursor whose read is not over ends when an index its plan reads
        // is dropped (mongod 8.2.11: 175 on the `getMore`).
        let reading = cursor.reading_collection();
        let cursors = ctx.cursors()?;
        let id = cursors
            .register_stream(ask.ns, Box::new(cursor))
            .map_err(|e| {
                CommandError::new(1, "InternalError", format!("cursor registry: {e:?}"))
            })?;
        if !ask.direct {
            cursors.set_reads(id, &format!("{}.{}", ask.db, ask.coll));
        }
        if reading && !indexes.is_empty() {
            cursors.set_plan_indexes(id, indexes);
        }
        id
    };
    let reply = doc! {
        "cursor": {
            "id": Bson::Int64(cursor_id),
            "ns": ask.ns,
        },
        "ok": 1.0,
    };
    Ok(Some(Opened {
        reply,
        batch: batch.docs,
        stats: crate::ExecStats {
            keys_examined: batch.keys_examined,
            docs_examined: batch.docs_examined,
            cursor_id,
            exhausted: batch.exhausted,
            ..Default::default()
        },
    }))
}

/// `ask`'s cursor, unread, and the indexes its plan reads, when its
/// pipeline streams. `buffered`: read through a cursor stage whatever the
/// planner takes (a `$unionWith`'s collection).
fn build(
    storage: &dyn Storage,
    ask: &Ask,
    buffered: bool,
) -> Result<Option<(AggStream, Vec<String>)>, CommandError> {
    if crate::ts_buckets::bucketed(storage, ask.db, ask.coll).is_some() {
        return Ok(None);
    }
    if storage
        .get_collection_options(ask.db, ask.coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return if buffered {
            Ok(None)
        } else {
            time_series(storage, ask)
        };
    }
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    if let Some(spec) = ask
        .pipeline
        .first()
        .and_then(Bson::as_document)
        .and_then(|d| d.get("$geoNear"))
    {
        if buffered || !ask.direct {
            return Ok(None);
        }
        return geo_near(storage, ask, spec);
    }
    // Under a collation, a DISTINCT_SCAN of an index with that collation
    // (entry format 6 keeps every index in mongod's order: RM-309).
    if !buffered {
        let grouped = crate::explain::distinct_group_read(
            storage,
            ask.db,
            ask.coll,
            ask.pipeline,
            ask.collation,
        )
        .map_err(read_error)?;
        if let Some(read) = grouped {
            return distinct_group(storage, ask, read);
        }
    }
    // A `$sort` the plan does not read in order blocks: `stream_plan` says.
    let planned = |s: &Bson| streams(s) || s.as_document().is_some_and(|d| d.contains_key("$sort"));
    if !ask.pipeline.iter().all(planned) {
        return Ok(None);
    }
    let query = crate::explain::pipeline_canonical(ask.coll, ask.pipeline);
    if !query.rest.iter().all(streams) {
        return Ok(None);
    }
    let hoisted = crate::explain::hoist::hoist_matches(ask.pipeline);
    // The read and the stages SBE takes run in its engine, as when the
    // pipeline is read whole; a pipeline SBE takes part of is read whole.
    let pushed =
        crate::explain::sbe_pushdown(storage, ask.db, ask.coll, &hoisted, ask.hint, ask.collation);
    let sbe = pushed.is_some();
    if sbe && pushed != Some(hoisted.len()) {
        return Ok(None);
    }
    let per_batch = !buffered && (query.rest.is_empty() || sbe);
    let limit = query.limit;
    // A plan two or more indexes contest is chosen by mongod's trial, run
    // through its plan cache when the command runs (RM-363, as a `find`'s
    // since RM-349): the pipeline's own read in the classic engine. The
    // query is the one mongod's plan cache keys the pipeline's front by.
    let tries = !buffered && !sbe;
    let order = crate::explain::order::Ask {
        db: ask.db,
        coll: ask.coll,
        filter: &query.filter,
        sort: query.sort.as_ref(),
        hint: ask.hint,
        collation: ask.collation,
        vars: ask.vars,
        limit: if limit > 0 { query.skip + limit } else { 0 },
        asked: crate::near::Asked {
            skip: query.skip,
            limit,
            sort: query.sort.as_ref(),
            projection: query.projection.as_ref(),
            cached: tries,
            ..Default::default()
        },
    };
    let planned = if tries {
        crate::explain::order::stream_tried(
            storage,
            &order,
            query.skip,
            limit,
            query.projection.as_ref(),
            false,
            Default::default(),
        )
    } else {
        crate::explain::order::stream_plan(
            storage,
            &order,
            query.skip,
            limit,
            query.projection.as_ref(),
            false,
            Default::default(),
        )
        .map(|plan| plan.map(|plan| (plan, None)))
    };
    let Some((plan, trial)) = planned.map_err(read_error)? else {
        return Ok(None);
    };
    // The projection the planner took is a stage of the pipeline: run as
    // written, not as `explain` prints it. mongod's plan runs it, so it is
    // not one of the stages after its `$cursor` stage.
    let in_plan = usize::from(query.projection_stage.is_some());
    // The stages after mongod's `$cursor` stage, as its `explain` lists
    // them, are the ones this cursor runs.
    let modelled = !buffered
        && !metas_or_sort_keys(&query.rest)
        && query
            .rest
            .iter()
            .all(|s| COUNTED_STAGES.contains(&stage_name(s)));
    let mut stages: Vec<Bson> = query.projection_stage.into_iter().collect();
    stages.extend(query.rest);
    // `{$meta: "recordId"}` / `{$meta: "indexKey"}` where mongod answers
    // them: read from fields each document is given as the read passes it
    // on, as the whole read gives them (`agg_meta`).
    let metas = if stages.iter().any(mentions_meta) {
        crate::agg_meta::rewrite(ask.pipeline, &mut stages)
    } else {
        Default::default()
    };
    // `{$meta: "sortKey"}` and a `$_internalSetWindowFields`'s ranks: from
    // the `$sort` the plan reads in order, worked out for each document as a
    // `$sort` stage leaves it (`agg_meta`).
    if stages.iter().any(crate::agg_meta::reads_sort_key) {
        match &query.sort {
            Some(sort) => {
                let mut with = vec![Bson::Document(doc! { "$sort": sort.clone() })];
                with.append(&mut stages);
                crate::agg_meta::rewrite_sort_metadata(&mut with);
                // The plan sorted: what follows the `$sort` stays.
                with.remove(0);
                stages = with;
            }
            None => crate::agg_meta::rewrite_sort_metadata(&mut stages),
        }
    }
    let opts = crate::explain::stream::Options {
        keys: metas.index_key,
        collation: ask.collation,
        ..Default::default()
    };
    let Some(mut source) =
        Stream::of_plan_with(&plan, ask.db, ask.coll, &query.filter, ask.vars, &opts)
            .map_err(read_error)?
    else {
        return Ok(None);
    };
    // What the trial's winner returned in it is buffered when the command
    // runs, and the read goes on from there; the first buffer counts what
    // the trial examined.
    if let Some(trial) = trial {
        let _active = secantus_core::collation::activate(ask.collation);
        source
            .buffer_trial(
                storage,
                trial.results,
                trial.ended,
                trial.cached.then_some(&plan),
            )
            .map_err(read_error)?;
    }
    // A `$unionWith`'s read counts none, as when the pipeline is read whole.
    if !buffered {
        crate::explain::order::count_index_use(storage, &order, crate::explain::order::Uses::Find);
    }
    let indexes = crate::explain::order::plan_indexes(storage, &order);
    if sbe && !stages.is_empty() {
        let end = stages.len();
        stages = secantus_core::sbe::mark(stages, 0, end);
    }
    let mut cursor = AggStream::new(
        Feed::Plan(Box::new(source)),
        (!per_batch).then_some(FIRST_BUFFER),
        stages,
        ask,
    );
    cursor.sbe = sbe;
    cursor.metas = (metas.record_id, metas.index_key);
    // A read the query layer runs whole checks as a `find` does; one
    // through the cursor stage counts the stages after it too, when they
    // are stages whose calls are counted here and run in the classic engine.
    cursor.modelled = modelled && (per_batch || !sbe);
    cursor.counted = cursor.modelled;
    cursor.in_plan = in_plan;
    Ok(Some((cursor, indexes)))
}

/// The stages after which mongod 8.2.11 still reads a time-series
/// collection through its cursor stage, 32 buckets first (measured
/// 2026-11-09). They need the whole measurement; once the pipeline needs
/// only some fields (an inclusion `$project`), mongod reads it otherwise,
/// one bucket at a time, and the pipeline is read whole here.
const TIME_SERIES_STAGES: &[&str] = &[
    "$match",
    "$addFields",
    "$set",
    "$unset",
    "$unwind",
    "$redact",
    "$skip",
    "$limit",
];

/// A `$project` that only leaves fields out.
fn excludes_only(spec: &Bson) -> bool {
    fn zero(v: &Bson) -> bool {
        match v {
            Bson::Boolean(b) => !b,
            Bson::Int32(n) => *n == 0,
            Bson::Int64(n) => *n == 0,
            Bson::Double(n) => *n == 0.0,
            Bson::Document(d) => !d.is_empty() && d.values().all(zero),
            _ => false,
        }
    }
    matches!(spec, Bson::Document(d) if !d.is_empty() && d.values().all(zero))
}

/// The cursor of a pipeline over a time-series collection: its buckets read
/// through the cursor stage, each unpacked as the cursor reaches it
/// (`ts_buckets::BucketScan`, RM-348). Read whole instead: under a hint or
/// a collation, when the filter names the meta field or the collection has
/// an index of its own (mongod could read an index over the buckets), and
/// for any other stage.
fn time_series(
    storage: &dyn Storage,
    ask: &Ask,
) -> Result<Option<(AggStream, Vec<String>)>, CommandError> {
    let fits = |s: &Bson| {
        let name = stage_name(s);
        TIME_SERIES_STAGES.contains(&name)
            || (name == "$project"
                && s.as_document()
                    .and_then(|d| d.get("$project"))
                    .is_some_and(excludes_only))
    };
    if ask.hint.is_some()
        || ask.collation.is_some()
        || !ask.pipeline.iter().all(fits)
        || ask.pipeline.iter().any(mentions_meta)
    {
        return Ok(None);
    }
    let hoisted = crate::explain::hoist::hoist_matches(ask.pipeline);
    if crate::explain::sbe_pushdown(storage, ask.db, ask.coll, &hoisted, None, None).is_some() {
        return Ok(None);
    }
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    let default_index = storage
        .get_collection_options(ask.db, ask.coll)
        .ok()
        .and_then(|o| crate::timeseries::Fields::of(&o))
        .and_then(|f| f.default_index())
        .map(|(name, _)| name);
    let own_index = storage
        .list_indexes(ask.db, ask.coll)
        .map_err(read_error)?
        .iter()
        .filter_map(|spec| spec.get_str("name").ok())
        .any(|name| name != "_id_" && Some(name) != default_index.as_deref());
    if own_index {
        return Ok(None);
    }
    let filter = crate::explain::pipeline_canonical(ask.coll, ask.pipeline).filter;
    let Some(scan) = crate::ts_buckets::BucketScan::of(storage, ask.db, ask.coll, ask.ns, &filter)?
    else {
        return Ok(None);
    };
    if scan.filters_meta() {
        return Ok(None);
    }
    let cursor = AggStream::new(
        Feed::Buckets(Box::new(scan)),
        Some(FIRST_BUFFER),
        ask.pipeline.to_vec(),
        ask,
    );
    Ok(Some((cursor, Vec::new())))
}

/// The cursor of a pipeline that opens with a `$geoNear`: mongod's ring
/// search, kept between buffers, each ring reading the collection as it is
/// when the search reaches it (RM-347), read through the cursor stage
/// (`DocumentSourceGeoNearCursor`), which gives each document its distance
/// as it buffers it. Anything after it that does not pass each document on
/// reads the whole pipeline at once, as before.
fn geo_near(
    storage: &dyn Storage,
    ask: &Ask,
    spec: &Bson,
) -> Result<Option<(AggStream, Vec<String>)>, CommandError> {
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    let rest = &ask.pipeline[1..];
    if !rest.iter().all(streams) || rest.iter().any(mentions_meta) {
        return Ok(None);
    }
    if !storage.can_stream(ask.db, ask.coll).map_err(read_error)? {
        return Ok(None);
    }
    let Some(read) = crate::aggregate::geo_near_read(spec, ask.db, ask.coll, storage) else {
        return Ok(None);
    };
    let Some(index) =
        crate::explain::geo::near_index(storage, ask.db, ask.coll, &read.key, &read.near)
            .map_err(read_error)?
    else {
        return Ok(None);
    };
    let Some((measure, base)) = crate::explain::geo::near_search(
        storage,
        ask.db,
        ask.coll,
        &read.query,
        &index,
        &read.near,
    )
    .map_err(read_error)?
    else {
        return Ok(None);
    };
    // The index use the whole read counts: the plan of the stage's
    // candidate read, when it has one.
    if let Some(filter) =
        crate::aggregate::geo_near_index_filter(spec, ask.db, Some(ask.coll), storage)
    {
        let order = crate::explain::order::Ask {
            db: ask.db,
            coll: ask.coll,
            filter: &filter,
            sort: None,
            hint: ask.hint,
            collation: ask.collation,
            vars: ask.vars,
            limit: 0,
            asked: Default::default(),
        };
        crate::explain::order::count_index_use(storage, &order, crate::explain::order::Uses::Find);
    }
    let source = Stream::of_near(ask.db, ask.coll, &read.query, ask.vars, measure, base, 0, 0);
    let mut cursor = AggStream::new(
        Feed::Plan(Box::new(source)),
        Some(FIRST_BUFFER),
        rest.to_vec(),
        ask,
    );
    cursor.geo_near = Some(read);
    Ok(Some((cursor, vec![index.name])))
}

/// The cursor of a pipeline that opens with a `$group` mongod answers from
/// a DISTINCT_SCAN: the scan's first document of each key, read through
/// the cursor stage, made the group's document as mongod's
/// `$groupByDistinctScan` makes it (a value the document lacks is null,
/// measured 8.2.11, 2026-10-25). A blocking stage after it reads them all.
fn distinct_group(
    storage: &dyn Storage,
    ask: &Ask,
    read: crate::explain::DistinctGroupRead,
) -> Result<Option<(AggStream, Vec<String>)>, CommandError> {
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    let crate::explain::DistinctGroupRead {
        plan,
        root,
        rest,
        filter,
    } = read;
    let Some(source) =
        Stream::of_plan(&plan, ask.db, ask.coll, &filter, ask.vars).map_err(read_error)?
    else {
        return Ok(None);
    };
    let index = scanned_index(&plan).unwrap_or_default();
    storage.record_index_use(ask.db, ask.coll, std::slice::from_ref(&index));
    let root: Document = root
        .into_iter()
        .map(|(k, v)| (k, Bson::Document(doc! { "$ifNull": [v, Bson::Null] })))
        .collect();
    let whole = !rest.iter().all(streams);
    let mut stages = vec![Bson::Document(doc! { "$replaceRoot": { "newRoot": root } })];
    stages.extend(rest);
    let mut cursor = AggStream::new(
        Feed::Plan(Box::new(source)),
        Some(FIRST_BUFFER),
        stages,
        ask,
    );
    if whole {
        cursor.unions.clear();
        cursor.whole = true;
    }
    Ok(Some((cursor, vec![index])))
}

/// The index a DISTINCT_SCAN plan reads.
fn scanned_index(plan: &crate::explain::Node) -> Option<String> {
    match plan {
        crate::explain::Node::DistinctScan(ix, _, _) => Some(ix.index.name.clone()),
        crate::explain::Node::Projection { input, .. } => scanned_index(input),
        _ => None,
    }
}

/// A stage this cursor runs as documents arrive, its spec naming no
/// `$meta` but a document's RecordId and index key, which the read keeps
/// (the other metadata the whole read attaches).
fn streams(stage: &Bson) -> bool {
    let Some((name, spec)) = stage.as_document().and_then(|d| d.iter().next()) else {
        return false;
    };
    STREAMING_STAGES.contains(&name.as_str()) && only_kept_meta(spec)
}

fn mentions_meta(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => d.iter().any(|(k, v)| k == "$meta" || mentions_meta(v)),
        Bson::Array(items) => items.iter().any(mentions_meta),
        _ => false,
    }
}

/// Every `$meta` in `v` asks for `recordId`, `indexKey` or `sortKey`.
fn only_kept_meta(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => d.iter().all(|(k, v)| {
            if k == "$meta" {
                matches!(v, Bson::String(s) if s == "recordId" || s == "indexKey" || s == "sortKey")
            } else {
                only_kept_meta(v)
            }
        }),
        Bson::Array(items) => items.iter().all(only_kept_meta),
        _ => true,
    }
}

/// Where an aggregate's cursor reads from: the collection's read, a
/// `$unionWith`'s pipeline read as it goes, or one read whole.
enum Feed {
    Plan(Box<Stream>),
    Buckets(Box<crate::ts_buckets::BucketScan>),
    Union(Box<AggStream>),
    Read(Vec<Vec<u8>>),
}

/// An aggregate's cursor: its collection read, and the stages after it run
/// on each document as the cursor is read.
struct AggStream {
    feed: Feed,
    /// The next cursor-stage buffer, in documents; `None`: the read is the
    /// batch's own, as many as the batch asks.
    buffer: Option<usize>,
    stages: Vec<Bson>,
    /// The `$skip` and `$limit` counts still to run, by stage.
    counts: Vec<Option<i64>>,
    /// The `$unionWith` stages not reached yet, by position: each one's
    /// documents run through the stages after it.
    unions: VecDeque<usize>,
    /// The first stage the feed's documents run through.
    start: usize,
    /// An `$unwind` can make several documents of one.
    multiplies: bool,
    /// Read and not yet run through the stages.
    read: VecDeque<Vec<u8>>,
    /// Through the stages and not yet returned.
    out: VecDeque<Vec<u8>>,
    /// The feed has nothing more to read.
    source_done: bool,
    db: String,
    coll: String,
    ns: String,
    vars: Document,
    command: Document,
    /// The plan, as every batch's line repeats it.
    summary: Option<String>,
    /// The read runs in SBE.
    sbe: bool,
    /// A stage blocks: every document is read, then run through the stages
    /// at once.
    whole: bool,
    /// The command's collation, which every batch reads and runs under.
    collation: Option<secantus_core::collation::Collation>,
    /// The collection or an index the read needs was dropped: mongod's 175
    /// message, met when the cursor next reads.
    gone: Option<String>,
    /// The aggregation memory budgets in force when it was opened (RM-211).
    budgets: secantus_core::memlimit::Budgets,
    /// Opened under `notablescan`: its namespace, which the stages that
    /// read other collections are checked under.
    notablescan: Option<String>,
    /// The documents read are given their RecordId and their index key, for
    /// the `$meta`s rewritten into reads of them (`agg_meta`).
    metas: (bool, bool),
    /// The read is a leading `$geoNear`'s, which gives each document what
    /// the stage adds to it.
    geo_near: Option<crate::aggregate::GeoNearRead>,
    /// Its stages are mongod's after its cursor stage, and run in the
    /// classic engine: each cursor-stage refill takes the global lock, as
    /// mongod's does, and a `getMore` takes none of its own (RM-360).
    modelled: bool,
    /// Its interrupt checks are made where mongod's are (RM-360): each
    /// cursor-stage refill, and each `getNext` of a stage after it.
    counted: bool,
    /// How many of the leading stages mongod's plan runs (the projection
    /// it took), which tick no interrupt checker.
    in_plan: usize,
    /// The pipeline interrupt checker's ticks to its next check, kept
    /// between batches as mongod's `ExpressionContext` is.
    ticks: u32,
    /// The `getNext` calls behind each document of `out`, counted when a
    /// batch returns it (see [`run_counted`](Self::run_counted)).
    out_calls: VecDeque<u64>,
    /// The calls each stage owes the next document it passes on.
    owed: Vec<u64>,
}

fn stage_name(s: &Bson) -> &str {
    s.as_document()
        .and_then(|d| d.keys().next())
        .map_or("", String::as_str)
}

impl AggStream {
    fn new(feed: Feed, buffer: Option<usize>, stages: Vec<Bson>, ask: &Ask) -> AggStream {
        let stages_len = stages.len();
        let counts = stages
            .iter()
            .map(|s| match stage_name(s) {
                "$skip" | "$limit" => s
                    .as_document()
                    .and_then(|d| d.values().next())
                    .and_then(crate::util::as_i64),
                _ => None,
            })
            .collect();
        AggStream {
            feed,
            buffer,
            multiplies: stages.iter().any(|s| stage_name(s) == "$unwind"),
            unions: (0..stages.len())
                .filter(|i| stage_name(&stages[*i]) == "$unionWith")
                .collect(),
            start: 0,
            counts,
            stages,
            read: VecDeque::new(),
            out: VecDeque::new(),
            source_done: false,
            db: ask.db.to_string(),
            coll: ask.coll.to_string(),
            ns: ask.ns.to_string(),
            vars: ask.vars.clone(),
            command: ask.command.clone(),
            summary: None,
            sbe: false,
            whole: false,
            collation: ask.collation.cloned(),
            gone: None,
            budgets: Default::default(),
            notablescan: None,
            metas: (false, false),
            geo_near: None,
            modelled: false,
            counted: false,
            in_plan: 0,
            ticks: secantus_core::deadline::FRESH_TICKS,
            out_calls: VecDeque::new(),
            owed: vec![0; stages_len],
        }
    }

    /// Whether this cursor reads through a cursor stage whose stages after
    /// it are counted (see [`COUNTED_STAGES`]).
    fn ticking(&self) -> bool {
        self.counted && self.buffer.is_some()
    }

    /// mongod's stages' `getNext` calls, `n` of them, each a tick of the
    /// pipeline's interrupt checker.
    fn tick(&self, n: usize) -> Result<(), CommandError> {
        if self.ticking() {
            secantus_core::deadline::tick_stages(n as u64)?;
        }
        Ok(())
    }

    /// Still reading the collection the cursor was opened on.
    fn reading_collection(&self) -> bool {
        self.start == 0 && !self.source_done
    }

    /// A `$limit` at or after `start` has passed all it will: documents
    /// that run from `start` go no further.
    fn limited(&self, start: usize) -> bool {
        (start..self.stages.len()).any(|i| {
            self.counts[i] == Some(0)
                && self.stages[i]
                    .as_document()
                    .is_some_and(|d| d.contains_key("$limit"))
        })
    }

    /// The pipeline of the `$unionWith` at `at`, opened now that the cursor
    /// has reached it: read through a cursor stage of its own when it
    /// streams over a collection (measured 8.2.11, 2026-10-25), else whole.
    fn open_union(&self, storage: &dyn Storage, at: usize) -> Result<Feed, CommandError> {
        let spec = self.stages[at]
            .as_document()
            .and_then(|d| d.get("$unionWith"))
            .cloned()
            .unwrap_or(Bson::Null);
        let (from, pipeline): (Option<&str>, Vec<Bson>) = match &spec {
            Bson::String(s) => (Some(s.as_str()), Vec::new()),
            Bson::Document(d) => (
                d.get_str("coll").ok(),
                d.get_array("pipeline").cloned().unwrap_or_default(),
            ),
            _ => (None, Vec::new()),
        };
        // A view, a default collation or a time-series collection is read
        // as the whole pipeline reads it.
        // Under `notablescan` too, whose check of the read is the whole
        // pipeline's.
        let plain = self.notablescan.is_none()
            && from.is_some_and(|c| {
                storage
                    .get_collection_options(&self.db, c)
                    .map_or(true, |o| {
                        !o.contains_key("viewOn")
                            && !o.contains_key("collation")
                            && !o.contains_key("timeseries")
                    })
            });
        if let (true, Some(from)) = (plain, from) {
            let ask = Ask {
                db: &self.db,
                coll: from,
                ns: &self.ns,
                pipeline: &pipeline,
                hint: None,
                vars: &Document::new(),
                batch_size: 0,
                command: &self.command,
                collation: self.collation.as_ref(),
                direct: false,
            };
            if let Some((mut inner, _)) = build(storage, &ask, true)? {
                inner.summary = None;
                return Ok(Feed::Union(Box::new(inner)));
            }
        }
        let _command = crate::aggregate::InCommand::enter(&self.ns);
        let _engine = secantus_core::sbe::enter(false);
        let _budgets = secantus_core::memlimit::with_budgets(self.budgets);
        let _spill = secantus_core::spill_table::with_tables(storage.spill_tables());
        let docs = crate::aggregate::run_segmented(
            Vec::new(),
            &[Bson::Document(doc! { "$unionWith": spec })],
            &self.db,
            Some(&self.coll),
            &self.vars,
            storage,
            self.collation.as_ref(),
            &self.command,
            None,
        )?;
        Ok(Feed::Read(crate::util::encode_docs(docs)?))
    }

    /// The feed's next documents: a cursor-stage buffer, or what the batch
    /// still wants (`left`, 0: as many as fit) when the read is the batch's.
    fn read_feed(
        &mut self,
        storage: &dyn Storage,
        left: usize,
        max_bytes: usize,
    ) -> Result<StreamedBatch, CommandError> {
        // What the cursor stage holds is returned; the next read fails
        // (measured 8.2.11, 2026-10-25: 31 more documents of a buffer of 32,
        // then 175, the whole `getMore`).
        if let Some(reason) = &self.gone {
            return Err(CommandError::new(175, "QueryPlanKilled", reason.clone()));
        }
        let ns = self.ns.clone();
        let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", &ns);
        match &mut self.feed {
            Feed::Plan(source) => {
                let engine = secantus_core::sbe::enter(self.sbe);
                let read = match self.buffer {
                    Some(n) => {
                        // mongod's cursor stage takes the collection again
                        // for each refill (`DocumentSourceCursor::loadBatch`):
                        // the global IS, its ticket, and the check the
                        // lock's grant makes (RM-360).
                        if self.modelled {
                            crate::lock_manager::global_read()?;
                        }
                        self.buffer = Some(n * 2);
                        source.next_buffer(storage, n, BUFFER_BYTES)
                    }
                    None => source.next_batch(storage, left, max_bytes),
                }
                .map_err(read_error)?;
                drop(engine);
                let (record_id, index_key) = self.metas;
                // mongod has an index key only for a read of one index scan.
                let index_key = index_key && source.single_scan();
                let docs = if let Some(geo) = &self.geo_near {
                    let decoded = crate::util::decode_docs(read.docs)?;
                    crate::util::encode_docs(
                        decoded
                            .into_iter()
                            .filter_map(|d| geo.annotate(d))
                            .collect(),
                    )?
                } else if record_id || index_key {
                    let mut decoded = crate::util::decode_docs(read.docs)?;
                    for (i, doc) in decoded.iter_mut().enumerate() {
                        if record_id {
                            doc.insert(crate::agg_meta::RECORD_ID, Bson::Int64(read.rids[i]));
                        }
                        if let (true, Some(key)) = (index_key, &read.keys[i]) {
                            doc.insert(crate::agg_meta::INDEX_KEY, Bson::Document(key.clone()));
                        }
                    }
                    crate::util::encode_docs(decoded)?
                } else {
                    read.docs
                };
                Ok(StreamedBatch {
                    docs,
                    exhausted: read.exhausted,
                    keys_examined: read.keys_examined,
                    docs_examined: read.docs_examined,
                })
            }
            Feed::Buckets(scan) => {
                let n = self.buffer.unwrap_or(FIRST_BUFFER);
                self.buffer = Some(n * 2);
                let read = scan.next_buffer(storage, n, BUFFER_BYTES)?;
                Ok(StreamedBatch {
                    docs: read.docs,
                    exhausted: read.exhausted,
                    keys_examined: 0,
                    docs_examined: read.examined,
                })
            }
            Feed::Union(inner) => inner.next_batch(storage, left, max_bytes),
            Feed::Read(docs) => Ok(StreamedBatch {
                docs: std::mem::take(docs),
                exhausted: true,
                keys_examined: 0,
                docs_examined: 0,
            }),
        }
    }

    /// Run `docs` through the stages from `start`, counting off each
    /// `$skip` and `$limit`; a `$unionWith` passes them on.
    fn run(
        &mut self,
        storage: &dyn Storage,
        docs: Vec<Document>,
    ) -> Result<Vec<Document>, CommandError> {
        let mut docs = docs;
        let mut run: Vec<Bson> = Vec::new();
        for i in self.start..self.stages.len() {
            if stage_name(&self.stages[i]) == "$unionWith" {
                continue;
            }
            let Some(left) = self.counts[i] else {
                run.push(self.stages[i].clone());
                continue;
            };
            docs = self.run_stages(storage, docs, &mut run)?;
            let skip = stage_name(&self.stages[i]) == "$skip";
            let n = (left.max(0) as usize).min(docs.len());
            if skip {
                docs.drain(..n);
            } else {
                docs.truncate(n);
            }
            self.counts[i] = Some(left - n as i64);
        }
        self.run_stages(storage, docs, &mut run)
    }

    /// [`run`](Self::run), one stage at a time, with the `getNext` calls
    /// that pulled each document out: mongod's pipeline pulls a document
    /// through a `getNext` of every stage it passes, the cursor stage's
    /// first, each a tick of its interrupt checker (RM-360). A document's
    /// calls are counted when a batch returns it, as mongod makes them when
    /// its cursor asks for it; a stage that takes in a document and passes
    /// none on (`$skip`, an `$unwind` of an empty array) owes its calls to
    /// the next document it passes on, or to the end of the pipeline.
    fn run_counted(
        &mut self,
        storage: &dyn Storage,
        docs: Vec<Document>,
    ) -> Result<Vec<(Document, u64)>, CommandError> {
        let mut items: Vec<(Document, u64)> = docs.into_iter().map(|d| (d, 1)).collect();
        for i in self.start..self.stages.len() {
            let own = u64::from(i >= self.in_plan);
            let mut owed = std::mem::take(&mut self.owed[i]);
            let mut passed: Vec<(Document, u64)> = Vec::with_capacity(items.len());
            match self.counts[i] {
                // `$skip` (a `$limit` is not counted: `COUNTED_STAGES`).
                Some(left) => {
                    let n = (left.max(0) as usize).min(items.len());
                    owed += items.drain(..n).map(|(_, t)| t).sum::<u64>();
                    self.counts[i] = Some(left - n as i64);
                    passed = items;
                }
                None if stage_name(&self.stages[i]) == "$unwind" => {
                    for (doc, t) in items {
                        let mut one = vec![self.stages[i].clone()];
                        let made = self.run_stages(storage, vec![doc], &mut one)?;
                        owed += t;
                        for d in made {
                            passed.push((d, std::mem::take(&mut owed)));
                        }
                    }
                }
                None => {
                    let ticks: Vec<u64> = items.iter().map(|(_, t)| *t).collect();
                    let docs = items.into_iter().map(|(d, _)| d).collect();
                    let mut one = vec![self.stages[i].clone()];
                    let made = self.run_stages(storage, docs, &mut one)?;
                    // The counted stages pass each document on.
                    if made.len() != ticks.len() {
                        self.counted = false;
                    }
                    passed = made.into_iter().zip(ticks).collect();
                }
            }
            for (_, t) in passed.iter_mut() {
                *t += own + std::mem::take(&mut owed);
            }
            self.owed[i] = owed;
            items = passed;
        }
        Ok(items)
    }

    /// The `getNext` calls that find the pipeline over: one of each stage,
    /// down to the cursor stage, and those the stages still owe.
    fn calls_at_eof(&self) -> u64 {
        1 + self.stages.len().saturating_sub(self.in_plan) as u64 + self.owed.iter().sum::<u64>()
    }

    fn run_stages(
        &self,
        storage: &dyn Storage,
        docs: Vec<Document>,
        stages: &mut Vec<Bson>,
    ) -> Result<Vec<Document>, CommandError> {
        let stages = std::mem::take(stages);
        if docs.is_empty() || stages.is_empty() {
            return Ok(docs);
        }
        let _command = crate::aggregate::InCommand::enter(&self.ns);
        // The engine markers switch to SBE and back.
        let _engine = secantus_core::sbe::enter(false);
        let _disk_use = secantus_core::memlimit::allow_disk_use(
            self.command.get_bool("allowDiskUse").unwrap_or(true),
        );
        let _budgets = secantus_core::memlimit::with_budgets(self.budgets);
        let _spill = secantus_core::spill_table::with_tables(storage.spill_tables());
        crate::aggregate::run_segmented(
            docs,
            &stages,
            &self.db,
            Some(&self.coll),
            &self.vars,
            storage,
            self.collation.as_ref(),
            &self.command,
            None,
        )
    }
}

impl StreamCursor for AggStream {
    fn next_batch(
        &mut self,
        storage: &dyn Storage,
        want: usize,
        max_bytes: usize,
    ) -> Result<StreamedBatch, CommandError> {
        let _active = secantus_core::collation::activate(self.collation.as_ref());
        let _notablescan = crate::notablescan::aggregate_on(self.notablescan.clone());
        // The pipeline's interrupt checker goes on from where the last
        // batch left it.
        if self.counted {
            secantus_core::deadline::resume_ticks(self.ticks);
        }
        secantus_core::deadline::take_expression_ticked();
        let (mut keys, mut examined) = (0, 0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let full = |out: &Vec<Vec<u8>>| want > 0 && out.len() >= want;
        // The pipeline ran out while filling the batch.
        let mut eof = false;
        'batch: loop {
            while let Some(blob) = self.out.pop_front() {
                // The calls that pulled it out are this batch's, even when
                // it is held for the next: mongod's cursor got it and keeps
                // it for the next batch.
                let calls = self.out_calls.pop_front().unwrap_or(0);
                self.tick(calls as usize)?;
                if !out.is_empty() && bytes + blob.len() > max_bytes {
                    self.out.push_front(blob);
                    self.out_calls.push_front(0);
                    break 'batch;
                }
                bytes += blob.len();
                out.push(blob);
                if full(&out) {
                    break 'batch;
                }
            }
            if self.limited(self.start) {
                self.read.clear();
                self.source_done = true;
            }
            if self.read.is_empty() {
                if self.source_done {
                    // The next `$unionWith`, opened as the cursor reaches it.
                    let Some(at) = self.unions.pop_front() else {
                        eof = true;
                        break;
                    };
                    if !self.limited(at + 1) {
                        self.feed = self.open_union(storage, at)?;
                        self.start = at + 1;
                        self.source_done = false;
                    }
                    continue;
                }
                let left = if want > 0 { want - out.len() } else { 0 };
                let read = self.read_feed(storage, left, max_bytes)?;
                keys += read.keys_examined;
                examined += read.docs_examined;
                self.source_done = read.exhausted;
                self.read.extend(read.docs);
                continue;
            }
            // A blocking stage runs once, over everything read.
            if self.whole {
                if !self.source_done {
                    let read = self.read_feed(storage, 0, max_bytes)?;
                    keys += read.keys_examined;
                    examined += read.docs_examined;
                    self.source_done = read.exhausted;
                    self.read.extend(read.docs);
                    continue;
                }
                let all = crate::util::decode_docs(self.read.drain(..).collect())?;
                let ran = self.run_stages(storage, all, &mut self.stages.clone())?;
                self.out_calls.extend(std::iter::repeat_n(0, ran.len()));
                self.out.extend(crate::util::encode_docs(ran)?);
                continue;
            }
            // As many read documents as the batch still wants (one at a
            // time behind an `$unwind`), so a document the batch does not
            // reach is not run yet.
            let take = match (want, self.multiplies) {
                (0, _) => self.read.len(),
                (_, true) => 1,
                _ => (want - out.len()).clamp(1, self.read.len()),
            };
            let chunk: Vec<Vec<u8>> = self.read.drain(..take).collect();
            let docs = crate::util::decode_docs(chunk)?;
            if self.ticking() {
                let ran = self.run_counted(storage, docs)?;
                self.out_calls.extend(ran.iter().map(|(_, calls)| *calls));
                self.out.extend(crate::util::encode_docs(
                    ran.into_iter().map(|(d, _)| d).collect(),
                )?);
            } else {
                let ran = self.run(storage, docs)?;
                self.out_calls.extend(std::iter::repeat_n(0, ran.len()));
                self.out.extend(crate::util::encode_docs(ran)?);
            }
        }
        if eof {
            self.tick(self.calls_at_eof() as usize)?;
        }
        // An expression that ticked the pipeline's interrupt checker
        // (`$map`, `$filter`, `$reduce`: one tick an element here, none on
        // mongod 8.2.11) leaves the count this cursor's from here on.
        if secantus_core::deadline::take_expression_ticked() {
            self.counted = false;
        }
        if self.counted {
            secantus_core::deadline::pipeline_counted();
            self.ticks = secantus_core::deadline::ticks_to_check();
        }
        // Through a cursor stage, mongod knows the pipeline is over only once
        // it asks for a document and gets none: a batch that fills exactly
        // leaves the cursor open (measured 8.2.11, 2026-10-25). A plan
        // executor's read is over when it reaches the end filling a batch.
        let exhausted = self.out.is_empty()
            && match self.buffer {
                Some(_) => eof,
                None => self.read.is_empty() && self.source_done && self.unions.is_empty(),
            };
        Ok(StreamedBatch {
            docs: out,
            exhausted,
            keys_examined: keys,
            docs_examined: examined,
        })
    }

    fn plan_summary(&self) -> Option<String> {
        self.summary.clone()
    }

    fn originating_command(&self) -> Document {
        self.command.clone()
    }

    fn source_gone(&mut self, reason: &str) -> bool {
        // A plan executor's read has nothing buffered.
        if self.buffer.is_none() {
            return false;
        }
        self.gone = Some(reason.to_string());
        true
    }

    fn locks_internally(&self) -> bool {
        self.modelled && self.buffer.is_some()
    }
}

/// An `aggregate` asked for a first batch of none, which mongod runs nothing
/// of until the first `getMore` (measured 8.2.11, 2026-10-25: its `$out` is
/// unwritten, its errors and its reads are the `getMore`'s). The cursor
/// holds the command, which that `getMore` runs (`cursors::deferred_get_more`).
pub(crate) struct Deferred {
    pub(crate) command: Document,
}

impl StreamCursor for Deferred {
    fn next_batch(
        &mut self,
        _storage: &dyn Storage,
        _want: usize,
        _max_bytes: usize,
    ) -> Result<StreamedBatch, CommandError> {
        Ok(StreamedBatch {
            docs: Vec::new(),
            exhausted: true,
            keys_examined: 0,
            docs_examined: 0,
        })
    }

    fn plan_summary(&self) -> Option<String> {
        None
    }

    fn originating_command(&self) -> Document {
        self.command.clone()
    }

    fn deferred(&self) -> Option<Document> {
        Some(self.command.clone())
    }
}

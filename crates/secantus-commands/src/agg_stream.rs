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
}

/// Open `ask`'s cursor, when its pipeline streams. `None`: read it whole.
pub(crate) fn open(ctx: &CommandContext, ask: &Ask) -> Result<Option<Opened>, CommandError> {
    if crate::notablescan::enabled(ctx) {
        return Ok(None);
    }
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
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    // A collation leaves no DISTINCT_SCAN over an uncollated index, and
    // this store's collated index is not in mongod's order (RM-309).
    if !buffered && ask.collation.is_none() {
        let grouped = crate::explain::distinct_group_read(storage, ask.db, ask.coll, ask.pipeline)
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
    let is_view = |from: &str| {
        storage
            .get_collection_options(ask.db, from)
            .is_ok_and(|o| o.get_str("viewOn").is_ok())
    };
    // The read and the stages SBE takes run in its engine, as when the
    // pipeline is read whole; a pipeline SBE takes part of is read whole.
    let pushed = secantus_core::sbe::pushdown(&hoisted, &is_view);
    let sbe = pushed.is_some();
    if sbe && pushed != Some(hoisted.len()) {
        return Ok(None);
    }
    let per_batch = !buffered && (query.rest.is_empty() || sbe);
    let limit = query.limit;
    let order = crate::explain::order::Ask {
        db: ask.db,
        coll: ask.coll,
        filter: &query.filter,
        sort: query.sort.as_ref(),
        hint: ask.hint,
        collation: ask.collation,
        vars: ask.vars,
        limit: if limit > 0 { query.skip + limit } else { 0 },
        asked: Default::default(),
    };
    let Some(plan) = crate::explain::order::stream_plan(
        storage,
        &order,
        query.skip,
        limit,
        query.projection.as_ref(),
        false,
    )
    .map_err(read_error)?
    else {
        return Ok(None);
    };
    let Some(source) = Stream::of_plan(storage, &plan, ask.db, ask.coll, &query.filter, ask.vars)
        .map_err(read_error)?
    else {
        return Ok(None);
    };
    // A `$unionWith`'s read counts none, as when the pipeline is read whole.
    if !buffered {
        crate::explain::order::count_index_use(storage, &order, crate::explain::order::Uses::Find);
    }
    let indexes = crate::explain::order::plan_indexes(storage, &order);
    // The projection the planner took is a stage of the pipeline: run as
    // written, not as `explain` prints it.
    let mut stages: Vec<Bson> = query.projection_stage.into_iter().collect();
    stages.extend(query.rest);
    if sbe && !stages.is_empty() {
        let end = stages.len();
        stages = secantus_core::sbe::mark(stages, 0, end);
    }
    let mut cursor = AggStream::new(source, (!per_batch).then_some(FIRST_BUFFER), stages, ask);
    cursor.sbe = sbe;
    Ok(Some((cursor, indexes)))
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
        Stream::of_plan(storage, &plan, ask.db, ask.coll, &filter, ask.vars).map_err(read_error)?
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
    let mut cursor = AggStream::new(source, Some(FIRST_BUFFER), stages, ask);
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
/// `$meta` (the metadata the whole read attaches).
fn streams(stage: &Bson) -> bool {
    let Some((name, spec)) = stage.as_document().and_then(|d| d.iter().next()) else {
        return false;
    };
    STREAMING_STAGES.contains(&name.as_str()) && !mentions_meta(spec)
}

fn mentions_meta(v: &Bson) -> bool {
    match v {
        Bson::Document(d) => d.iter().any(|(k, v)| k == "$meta" || mentions_meta(v)),
        Bson::Array(items) => items.iter().any(mentions_meta),
        _ => false,
    }
}

/// Where an aggregate's cursor reads from: the collection's read, a
/// `$unionWith`'s pipeline read as it goes, or one read whole.
enum Feed {
    Plan(Box<Stream>),
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
}

fn stage_name(s: &Bson) -> &str {
    s.as_document()
        .and_then(|d| d.keys().next())
        .map_or("", String::as_str)
}

impl AggStream {
    fn new(source: Stream, buffer: Option<usize>, stages: Vec<Bson>, ask: &Ask) -> AggStream {
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
            feed: Feed::Plan(Box::new(source)),
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
        }
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
        let plain = from.is_some_and(|c| {
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
                        self.buffer = Some(n * 2);
                        source.next_buffer(storage, n, BUFFER_BYTES)
                    }
                    None => source.next_batch(storage, left, max_bytes),
                }
                .map_err(read_error)?;
                drop(engine);
                Ok(StreamedBatch {
                    docs: read.docs,
                    exhausted: read.exhausted,
                    keys_examined: read.keys_examined,
                    docs_examined: read.docs_examined,
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
        let (mut keys, mut examined) = (0, 0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let full = |out: &Vec<Vec<u8>>| want > 0 && out.len() >= want;
        // The pipeline ran out while filling the batch.
        let mut eof = false;
        'batch: loop {
            while let Some(blob) = self.out.pop_front() {
                if !out.is_empty() && bytes + blob.len() > max_bytes {
                    self.out.push_front(blob);
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
            let ran = self.run(storage, docs)?;
            self.out.extend(crate::util::encode_docs(ran)?);
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

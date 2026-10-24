//! An `aggregate` cursor that reads on as `getMore` asks (RM-080).
//!
//! mongod reads an aggregate's collection one of two ways (measured 8.2.11,
//! 2026-10-24). A pipeline its query planner takes whole, or whose stages it
//! pushes into SBE with the read, is a plan executor read batch by batch, as
//! a `find` is. Any other reads through its cursor stage, which buffers the
//! query's documents: 32 at first, twice as many at each refill, and no more
//! once a buffer holds 4 MB. The stages after it run on each buffered
//! document only as the cursor needs it, so an error in a later document is
//! the `getMore`'s. A pipeline with a stage that blocks (`$group`, an
//! unindexed `$sort`) reads everything for its first batch, and is not
//! streamed here.

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
}

/// An opened cursor: the reply, and the first batch that goes in it.
pub(crate) struct Opened {
    pub reply: Document,
    pub batch: Vec<Vec<u8>>,
}

/// Open `ask`'s cursor, when its pipeline streams. `None`: read it whole.
pub(crate) fn open(ctx: &CommandContext, ask: &Ask) -> Result<Option<Opened>, CommandError> {
    // A `$sort` the plan does not read in order blocks: `stream_plan` says.
    let planned = |s: &Bson| streams(s) || s.as_document().is_some_and(|d| d.contains_key("$sort"));
    if !ask.pipeline.iter().all(planned) || crate::notablescan::enabled(ctx) {
        return Ok(None);
    }
    let storage = ctx
        .storage
        .clone()
        .ok_or_else(|| CommandError::new(1, "InternalError", "no storage configured"))?;
    let storage = storage.as_ref();
    if crate::ts_buckets::bucketed(storage, ask.db, ask.coll).is_some() {
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
    let per_batch = query.rest.is_empty() || sbe;
    let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", ask.ns);
    let limit = query.limit;
    let order = crate::explain::order::Ask {
        db: ask.db,
        coll: ask.coll,
        filter: &query.filter,
        sort: query.sort.as_ref(),
        hint: ask.hint,
        collation: None,
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
    crate::explain::order::count_index_use(storage, &order, crate::explain::order::Uses::Find);
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
    cursor.summary = crate::explain::summary::plan_summary(ask.command, ctx);
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
        let indexes = if cursor.source_done {
            Vec::new()
        } else {
            crate::explain::order::plan_indexes(storage, &order)
        };
        let cursors = ctx.cursors()?;
        let id = cursors
            .register_stream(ask.ns, Box::new(cursor))
            .map_err(|e| {
                CommandError::new(1, "InternalError", format!("cursor registry: {e:?}"))
            })?;
        if !indexes.is_empty() {
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

/// An aggregate's cursor: its collection read, and the stages after it run
/// on each document as the cursor is read.
struct AggStream {
    source: Stream,
    /// The next cursor-stage buffer, in documents; `None`: the read is the
    /// batch's own, as many as the batch asks.
    buffer: Option<usize>,
    stages: Vec<Bson>,
    /// The `$skip` and `$limit` counts still to run, by stage.
    counts: Vec<Option<i64>>,
    /// An `$unwind` can make several documents of one.
    multiplies: bool,
    /// Read and not yet run through the stages.
    read: VecDeque<Vec<u8>>,
    /// Through the stages and not yet returned.
    out: VecDeque<Vec<u8>>,
    source_done: bool,
    /// A `$limit` has passed all it will.
    limited: bool,
    db: String,
    coll: String,
    ns: String,
    vars: Document,
    command: Document,
    /// The plan, as every batch's line repeats it.
    summary: Option<String>,
    /// The read runs in SBE.
    sbe: bool,
}

impl AggStream {
    fn new(source: Stream, buffer: Option<usize>, stages: Vec<Bson>, ask: &Ask) -> AggStream {
        let name = |s: &Bson| {
            s.as_document()
                .and_then(|d| d.keys().next().cloned())
                .unwrap_or_default()
        };
        let counts = stages
            .iter()
            .map(|s| match name(s).as_str() {
                "$skip" | "$limit" => s
                    .as_document()
                    .and_then(|d| d.values().next())
                    .and_then(crate::util::as_i64),
                _ => None,
            })
            .collect();
        AggStream {
            source,
            buffer,
            multiplies: stages.iter().any(|s| name(s) == "$unwind"),
            counts,
            stages,
            read: VecDeque::new(),
            out: VecDeque::new(),
            source_done: false,
            limited: false,
            db: ask.db.to_string(),
            coll: ask.coll.to_string(),
            ns: ask.ns.to_string(),
            vars: ask.vars.clone(),
            command: ask.command.clone(),
            summary: None,
            sbe: false,
        }
    }

    /// Run `docs` through the stages, counting off each `$skip` and `$limit`.
    fn run(
        &mut self,
        storage: &dyn Storage,
        docs: Vec<Document>,
    ) -> Result<Vec<Document>, CommandError> {
        let mut docs = docs;
        let mut run: Vec<Bson> = Vec::new();
        for i in 0..self.stages.len() {
            let Some(left) = self.counts[i] else {
                run.push(self.stages[i].clone());
                continue;
            };
            docs = self.run_stages(storage, docs, &mut run)?;
            let skip = self.stages[i]
                .as_document()
                .is_some_and(|d| d.contains_key("$skip"));
            let n = (left.max(0) as usize).min(docs.len());
            if skip {
                docs.drain(..n);
            } else {
                docs.truncate(n);
                self.limited |= left - n as i64 == 0;
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
        crate::aggregate::run_segmented(
            docs,
            &stages,
            &self.db,
            Some(&self.coll),
            &self.vars,
            storage,
            None,
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
        let ns = self.ns.clone();
        let read_error = |e| crate::util::read_error_on_namespace(e, "aggregate", &ns);
        let (mut keys, mut examined) = (0, 0);
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut bytes = 0usize;
        let full = |out: &Vec<Vec<u8>>| want > 0 && out.len() >= want;
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
            if self.limited {
                self.read.clear();
                break;
            }
            if self.read.is_empty() {
                if self.source_done {
                    break;
                }
                let engine = secantus_core::sbe::enter(self.sbe);
                let read = match self.buffer {
                    Some(n) => {
                        self.buffer = Some(n * 2);
                        self.source.next_buffer(storage, n, BUFFER_BYTES)
                    }
                    None => {
                        let left = if want > 0 { want - out.len() } else { 0 };
                        self.source.next_batch(storage, left, max_bytes)
                    }
                }
                .map_err(read_error)?;
                drop(engine);
                keys += read.keys_examined;
                examined += read.docs_examined;
                self.source_done = read.exhausted;
                self.read.extend(read.docs);
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
        let exhausted =
            self.out.is_empty() && (self.limited || (self.source_done && self.read.is_empty()));
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
}

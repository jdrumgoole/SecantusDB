# Streaming cursors for the Rust MongoDB server (RM-080, RM-023)

Decided 2026-10-24 (Joe): build the real mechanism -- a cursor that reads on
as `getMore` asks, with keys and documents counted as they are read -- before
RM-023's per-batch counts. This file is the plan; the backlog items hold what
is open.

## What mongod does (measured 8.2.11, 2026-10-24)

`tools`-less probe, recorded in `tests/backlog/` as each phase lands:

- Between batches mongod's executor SAVES its position and each `getMore`
  RESTORES it on a fresh snapshot, fetching each document as it is now:
  - collection scan: a document inserted after the position appears, one
    deleted ahead of it does not;
  - index scan: the position is (key, RecordId). A key moved behind the
    cursor is not returned; one moved ahead is returned again (a document
    can come back twice);
  - a blocking stage (in-memory sort, `$sort`, `$group`) reads everything
    for the first batch, so later writes are invisible; a sort an index
    provides streams;
  - `$match` / `$project` pipelines stream;
  - a cursor in a transaction reads the transaction's snapshot;
  - dropping the collection / the index / the database: 175.
- Counts are per batch, as read, with no lookahead: an index scan of 6 with
  `batchSize: 2` examines 2, 2, 2, then 0 on the getMore that finds the end
  (`cursorExhausted: true`); a filtered collection scan examines just enough
  documents to fill its batch.
- Every `getMore` line repeats the cursor's `planSummary`.

## The design

mongod's save/restore model, on this server's constraints (WiredTiger
sessions are per call and thread-affine; a `getMore` arrives on any
connection thread; no WT cursor may outlive its call):

- A streaming cursor holds a PLAN STATE, not documents: the plan (from
  `explain::plan`), its position (a RecordId for a collection scan; a key
  and RecordId for an index scan), the RecordIds already returned by a
  multikey scan (mongod de-duplicates across batches), the remaining
  skip/limit, the projection and the counters.
- Each batch is read in the call that asks for it, through `op_session()`,
  so inside a transaction it reads the transaction's snapshot.
- The executor is `explain/exec.rs`'s `Scan` semantics (bounds, seeks,
  `keysExamined`, de-duplication) run over the store's REAL index entries,
  and FETCH reads each document by RecordId as it is now.
- A compound index entry is one byte string; `sortkey` encodings are
  self-delimiting, so a parser (`sortkey::value_len`) splits it into the
  per-field keys the bounds work on. (Splitting at `COMPOUND_SEP` is not
  sound: a negative number's digit pair 99 encodes as `0x00`.)
- Anything not yet streamable keeps today's path (read all, buffer the
  rest), with the reason in the code.

## Phases (each its own PR, green on its own)

1. **Streaming `find`** (done 2026-10-24, branch `rust-mdb-rm023`). `sortkey::value_len` / split + fuzz tests;
   storage primitives (`index_entries_from`, `records_by_id`,
   `records_after`) on the trait and adapter; `commands/stream.rs` for
   COLLSCAN and FETCH(IXSCAN) plans over ascending, uncollated indexes,
   with skip, limit and projection; a non-tailable cursor producer read
   outside the registry lock; per-batch `keysExamined` / `docsExamined`,
   `getMore`'s `planSummary` and `cursorExhausted` on the slow line and
   profile entries. Fixes RM-080's gap test.
2. **The rest of `find`.**
   - Done 2026-10-24 (branch `rust-mdb-stream2`): backward index scans
     (each field's key compared reversed, `exec::reversed_key`), `$natural`
     backward, the `_id` index (the documents' `id_key`s, walked in
     `NAT_SEQ_TABLE`; not for a collection holding an `_id` whose key keeps
     entry format 4's bytes), covered plans (documents read to project,
     not counted), a LIMIT above the projection.
   - Done 2026-10-27 (branch `rust-mdb-rm080-streaming-cursors`): the
     stream is a small executor of the plan's stages (COLLSCAN, IXSCAN,
     FETCH, SKIP, OR, SORT_MERGE, a SORT of index keys below the FETCH),
     each yielding at the end of a batch. Entry format 6 keys a descending
     field inverted and terminated (`sortkey::DESC_END`), an empty leaf
     array `undefined` and an element lacking the leaf null, so every
     index's entries are mongod's keys in mongod's order (plus this store's
     whole-array keys, which a multikey scan passes over by the document's
     own keys); `_id` key format 2 keys every `_id` in mongod's order. A
     collated index needed no change: its entries were already the
     collation's sort keys. Descending, multikey and collated scans, OR,
     SORT_MERGE (and the planner's `SORT_MERGE` for a rooted `$or` whose
     branches give the sort), `min`/`max`, `returnKey` / `showRecordId` and
     the sort of keys (whose documents are fetched as a batch reaches them,
     as mongod's are) stream. A sort of whole documents is read whole for
     the first batch, as mongod reads it.
   - Left: a plan mongod picks by trial (RM-349), `$near` (RM-347).
3. **`aggregate`.**
   - Done (2026-10-24, branch `rust-mdb-stream3`, `agg_stream.rs`): a
     pipeline of `$match`, `$project`, `$unset`, `$addFields` / `$set`,
     `$replaceRoot` / `$replaceWith`, `$unwind`, `$redact`, `$skip`,
     `$limit`, `$lookup` and an in-order `$sort` streams. mongod reads it
     batch by batch when its planner takes the pipeline whole or SBE takes
     the rest (`$lookup`), else through the cursor stage's buffer: 32
     documents, doubling at each refill, a refill ending at the document
     that brings it to 4 MB (`Stream::next_buffer`); the stages run on each
     buffered document only as the cursor reaches it, so an error in a later
     one is the `getMore`'s. `explain`'s match hoist now moves a `$match`
     ahead of a `$project` / `$unset` that keeps its fields and ahead of a
     `$sort` (RM-307), which is what makes `[$project, $match]` a
     whole-pipeline read.
   - Done (2026-10-25, branch `rust-mdb-stream4`): `$unionWith` passes the
     collection's documents on, then opens its own collection when the
     cursor reaches it and reads it through a cursor stage of its own
     (`Feed::Union`; a view, collated or time-series target is read whole
     at that point). A `$group` mongod answers from a DISTINCT_SCAN
     (`$groupByDistinctScan`) streams over the scan (`IndexScan::distinct`),
     its groups in the scan's order, after `explain`'s rule was corrected
     (RM-309: all `$first` or all `$last`, `$last` flips the scan, the
     covering index with the fewest fields). A first batch of none runs
     nothing: the cursor holds the command and its first `getMore` runs
     it (`agg_stream::Deferred`, `cursors::deferred_get_more`). A batch
     that fills exactly leaves a cursor-stage cursor open, as on mongod.
   - Done (2026-10-25, branch `rust-mdb-stream5`): a `find` or an
     `aggregate` through a view or under a collation streams as over a
     collection (the collation made active for every batch), and a collated
     index streams when its bounds hold only numbers or dates. A view's
     cursor is killed by drops of the collection beneath it
     (`CursorRegistry::set_reads`), not of the view; a cursor-stage cursor
     whose collection or index is dropped returns what it buffered and
     fails at the next read (`StreamCursor::source_gone`), as on mongod.
   - Done (2026-10-27, branch `rust-mdb-rm080-streaming-cursors`): a
     collated index bounded on strings, a DISTINCT_SCAN `$group` down a
     descending or along a collated index (RM-309), `notablescan` (its
     stages that read other collections checked as each batch runs them),
     `{$meta: "recordId"}`, `"indexKey"` and `"sortKey"`.
   - Done (2026-10-27, branch `rust-mdb-rm023-slow-query-counts`, RM-023):
     `keysExamined` / `docsExamined` on the `aggregate` command's own line
     (streamed or read whole); `count` (a COUNT_SCAN reads index entries
     alone and counts the call that finds its end, `Stream::of_count`),
     `distinct` (a DISTINCT_SCAN answers from its keys), a write
     statement's and `findAndModify`'s read of its documents
     (`order::write_selection`: every match for a write of many, up to the
     first for a write of one, then the write takes those RecordIds), a
     sort of whole documents (`Stage::SortDocs`, read whole for the first
     batch and held) and an `_id` or unique-index point read
     (`Stage::Point`).
   - Left: a time-series collection (RM-348), `$geoNear` (RM-347), a plan
     chosen by trial (RM-349); their counts (RM-363).
4. **`storage` on the line.** Done (2026-10-27, RM-023): WiredTiger's
   per-session statistics, read from each session the operation closes and
   from a transaction's session at each statement's end
   (`secantus_wt::operation_stats_scope`), with the index keys a write
   inserted and deleted (`secantus_storage::operation_scope`).

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
   - Left: descending and collated indexes (their stored order is not
     mongod's: a descending string field's inverted bytes leave `"a"` before
     `"ab"`, and a collated string keys by a one-level fold -- an
     entry-format change, which needs its own decision), multikey (the
     RecordId set across batches; this store also writes whole-array keys,
     and an element that is an array cannot be told from one), OR plans,
     SORT_MERGE, `min`/`max`, `returnKey` / `showRecordId`, blocking sorts
     (read all for the first batch, counted as read; the sort is the store's
     comparator, private to `secantus-storage`).
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
   - Left: a collation, a view, a time-series collection, `$meta`,
     `$geoNear`, `notablescan`; a DISTINCT_SCAN `$group` down a descending
     or along a collated index, or through a view (RM-309); `keysExamined`
     / `docsExamined` on the `aggregate`
     command's own line; counts for `count`, `distinct`, write statements
     and `findAndModify` from their reads.
4. **`storage` on the line.** WiredTiger's per-session statistics
   (`timeWaitingMicros`, bytes read and written) for the operation (the last
   part of RM-023).

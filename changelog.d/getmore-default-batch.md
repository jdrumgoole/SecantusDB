### A full scan now drains in two round trips, like mongod

MongoDB's 101-document cursor default applies only to a query's first
batch — a `getMore` with no `batchSize` fills its reply with as many
documents as fit in 16MB. Both servers were reusing the 101-document
default on every `getMore`, so draining a collection cost one round
trip per 101 documents; a 10,000-document full scan paid ~100 round
trips where mongod pays two. That round-trip tax was the entirety of
the benchmark's remaining full-scan gap to mongod: with the corrected
default the Rust server's `find` full scan lands at parity with
mongod on the same box.

Both servers now fill an unspecified `getMore` batch up to mongod's
16MB budget (always at least one document, so a drain makes
progress), and an explicit `batchSize` is byte-capped the same way —
a batch stops before the document that would push the reply past
16MB, and the cursor stays open for the remainder. Tailable
change-stream cursors keep the small incremental default.

#### Fixed

- `getMore` without `batchSize` returned 101 documents per batch on
  both servers instead of filling the reply to mongod's 16MB budget —
  a full collection scan paid `count / 101` wire round trips instead
  of ~2. (`crates/secantus-commands/src/cursors.rs`,
  `src/secantus/commands.py::_get_more`)
- A `getMore` with an explicit `batchSize` could assemble a reply of
  unbounded size; batches are now capped at 16MB of documents with
  the cursor kept open for the remainder, matching mongod.

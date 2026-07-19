### The Rust server stops decoding documents just to re-encode them for the wire

The Rust server used to decode every document it served into an owned
`bson::Document` purely to build the wire reply — then immediately
re-encode it. The storage scan and the cursor registry already speak raw
BSON bytes end-to-end, so a `find`'s `firstBatch` and every `getMore`'s
`nextBatch` were round-tripping through a decode→`IndexMap`→re-encode step
that produced exactly the bytes they started from. That reply-path
materialization was one of the two dominant hot spots the profiler found
(`tasks/rust-perf-findings.md`).

Cursor replies now splice the pre-encoded document blobs straight onto the
wire. A new `secantus_wire::encode_cursor_reply` assembles the
`cursor.firstBatch` / `cursor.nextBatch` BSON array from the stored blobs
without decoding them, and the `find` (no-projection) and non-tailable
`getMore` handlers hand their batches to the server as raw bytes instead
of an owned array. The output is byte-for-byte identical to the old path
(pinned by a unit test), so no driver can tell the difference — the work
saved is pure overhead. This is Phase 1 of the raw-BSON serving-path plan;
the change-stream (tailable) path, projected `find`, and exhaust-cursor
streaming keep their existing behaviour for now.

#### Changed

- Rust server: `find` (without a projection) and non-tailable `getMore`
  no longer materialize their document batch into the reply — the
  pre-encoded blobs are spliced onto the wire by
  `secantus_wire::encode_cursor_reply`, eliminating the reply-path
  decode→re-encode round-trip. The batch is carried to the server
  out-of-band via `CommandContext::pending_batch` (the same idiom as
  `close_connection`); the exhaust-getMore streamer reconstructs the
  batch it needs to reframe.

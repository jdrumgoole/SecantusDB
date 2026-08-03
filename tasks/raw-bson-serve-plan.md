# Raw-BSON serve path — CLOSED: already shipped (correction, 2026-08-03)

**The plan this file previously held described work that had already landed.**
It was written from the perf-findings narrative (Findings 1+2 "the scan path
materializes every document / the reply path materializes them again", with
Finding 11 noting only the *insert* raw path had shipped) without verifying
the current code. A code audit shows every slice of the planned program is
live on `main`:

- **S1 (raw COLLSCAN filter)** — `secantus_core::query::matches_raw` filters
  over `RawDocument`, decoding only the fields the filter reaches, with a
  documented fallback signal for `$expr` / `$jsonSchema` / parse hiccups; the
  storage `find_matching_with` post-filter loop uses it, and `find({})` skips
  per-doc matching entirely (`crates/secantus-storage/src/lib.rs`, "Filter
  over RAW BSON" block).
- **S2 (verbatim reply batches)** — the find handler keeps `Vec<Vec<u8>>`
  end-to-end; no-projection batches go to the wire via `encode_cursor_reply`
  "with no decode→re-encode round-trip" (`crates/secantus-commands/src/find.rs`).
- **S3 (IXSCAN fetch)** — index candidates return blobs
  (`docs_by_recordids`) and flow through the same raw filter loop.
- **S4 (projection-on-raw)** — `projection::apply_projection_raw` with a
  per-doc fallback to the owned projector for exotic specs.
- **S5 (aggregation leading `$match`)** — the lifted fetch filter also runs
  `matches_raw` (`crates/secantus-commands/src/aggregate.rs`).

The raw matcher is pinned bool-for-bool against the owned matcher by the
parity suite (per its module docs), and the owned matcher remains the
semantic oracle.

## What the correction leaves open

The benchmark's `find full scan` row (2.3× mongod) **post-dates** the raw
path, so the residual is NOT the materialization tax. A leaf-weighted
`sample` of the daemon under a sustained single-client full-scan load
(2026-08-03, 10k mixed docs, ~56 scans/s ≈ the benchmark rate) shows no
server-side CPU hotspot: the busy leaves are the WiredTiger cursor walk
(`__curfile_next` / `__pack_next` / `__unpack_read` / `__wt_txn_read_upd_list`),
modest `memmove` (blob copies into batches), and socket syscalls — the
connection thread spends most wall time in `recvfrom`, i.e. waiting for the
client's next getMore. The residual full-scan gap is **latency-shaped**
(per-batch round-trip composition: batch sizing, reply assembly, socket
write path), not a server CPU tax.

**Next investigation (a dedicated slice, measurement-first):** decompose a
full scan's wall time into client decode / wire round-trips / server
per-batch assembly — e.g. timestamped per-getMore server logs vs client
timings, and an A/B on internal batch size — before touching any code.
Candidate micro-levers if the decomposition points at the server: batch
`memmove` coalescing (write blobs straight into the reply frame), socket
write batching (`writev` for header + batch), and the WT cursor-walk copy
(`get_value_u` allocates per row).

## Non-goals (measured dead ends — do not revisit without new evidence)

- App-level caches above the WT cursor cache (Finding 18: measured zero).
- Write-path CPU micro-surgery (Finding 11: lock-free counters −2%, oplog
  splice +1% noise).

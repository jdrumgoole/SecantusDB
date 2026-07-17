### The Rust server's concurrency story, measured honestly

With the per-collection write locks landed, we re-measured what they
actually bought — and the headline is a correction. The
per-collection split did **not** improve multi-writer insert throughput:
the Rust server still holds at roughly half its single-writer aggregate
rate as writers are added, statistically unchanged from before. That is
the useful result. The global storage lock was never the constraint on
write throughput; the ceiling is inside WiredTiger itself (the shared
oplog table's append hotspot, cache and checkpoint), the same wall a
pure-C pthread benchmark hits with no Rust lock in the picture. Lifting
write scaling needs a scheduler above WiredTiger — which is what a real
`mongod` has and remains explicitly out of scope for a single-node
surrogate.

What the split *did* buy, and what the write-throughput curve doesn't
show, is read concurrency and write correctness: a read-heavy workload now
keeps 60–75% of its standalone query throughput while eight writers
saturate the server, where before every read queued behind every write.
`docs/concurrency.md` and `tasks/rust-perf-findings.md` carry the measured
numbers and the corrected attribution.

#### Added

- Rust server: `findAndModify` logs a warning every few seconds of
  continuous re-picking (a concurrent writer repeatedly stealing the
  matched document), so a steal storm on a hot job-queue document is
  visible in the server log instead of surfacing only as CPU — mirroring
  the storage layer's write-conflict retry telemetry.
- CI: the Rust parametrization of the `#451` concurrency stress suite
  (exactly-one-winner races, exact final counts, typed-errors-only) now
  runs in the `storage-engine` job — previously it `importorskip`ed in
  every lane because no other job builds the embedded Rust server.

#### Changed

- `docs/concurrency.md` corrects the Rust-server explanation: the flat
  multi-writer curve is a WiredTiger ceiling, not the (now removed) global
  storage lock, and records the read-under-write-load retention the
  per-collection split delivered.

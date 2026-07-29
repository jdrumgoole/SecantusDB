### Rust concurrency: steal telemetry, a read-under-load bench, and the CI that runs it

The per-collection write-lock work turned the Rust server's multi-writer
story into real scaling — it climbs to about 2.6× its single-writer rate
at four concurrent writers before a WiredTiger ceiling (specifically the
oplog's WAL append and checkpoint share) bends the curve back down, and
the opt-in async + non-logged oplog stack lifts even that to a monotonic
~2.4× at eight writers. That ceiling lives inside WiredTiger, not in a
SecantusDB lock; `docs/concurrency.md` carries the measured curve and the
attribution. This slice adds the tooling and telemetry *around* that
result rather than the measurement itself.

The new `bench/read_concurrency.py` harness measures the property the lock
split most directly buys and that a raw write-throughput curve hides: a
read-heavy workload keeps 60–75% of its standalone query throughput while
eight writers saturate the server, where before every read queued behind
every write. The `findAndModify` steal telemetry makes a concurrent-steal
storm on a hot job-queue document visible in the server log instead of
surfacing only as CPU. And the Rust parametrization of the `#451`
concurrency stress suite now actually runs in CI, where it previously
skipped itself in every lane.

#### Added

- Rust server: `findAndModify` logs a warning every few seconds of
  continuous re-picking (a concurrent writer repeatedly stealing the
  matched document), so a steal storm on a hot job-queue document is
  visible in the server log instead of surfacing only as CPU — mirroring
  the storage layer's write-conflict retry telemetry.
- `bench/read_concurrency.py`: a read-under-write-load benchmark for the
  Rust server — measures the query throughput a read-heavy client retains
  while N writers saturate the server.
- CI: the Rust parametrization of the `#451` concurrency stress suite
  (exactly-one-winner races, exact final counts, typed-errors-only) now
  runs in the `storage-engine` job — previously it `importorskip`ed in
  every lane because no other job builds the embedded Rust server.

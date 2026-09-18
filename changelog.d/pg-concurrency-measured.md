### The Rust PostgreSQL server's scaling problem was not the one on the list

A backlog item had stood for over a week saying the Rust PostgreSQL server
serialised its writes behind a global mutex, and that removing it was the next
piece of performance work. Reading the twenty-two places that mutex is taken
shows what it actually guards: checkpoints, archives, collection options,
renames, index builds and catalog records. Not one row insert, update or delete
takes it. The single site near a write path holds it just long enough to open a
storage session, and per-connection transactions — the thing the entry proposed
building — were already there.

Measuring says the same. Against PostgreSQL 16 on the same machine, write
throughput scales at 2.14x on four connections where PostgreSQL manages 2.36x,
and putting every client on one contended table costs this server almost
nothing extra. The read path, which takes no lock at all, behaves the same way.
There is no collapse to fix.

What the measurements do show is a statement that costs too much. A single
uncontended connection spends about sixty-six microseconds of server CPU on a
`SELECT` that PostgreSQL serves in thirteen — roughly five times the cost,
before any concurrency is involved, and that ratio is most of the gap at every
connection count. It is a profiling problem, not a locking one, and the item
now says so. The benchmark that settled it ships as `invoke pg-concurrency` so
the next person re-measures rather than re-reasons.

#### Added

- `bench/pg_concurrency.py` and `invoke pg-concurrency`: statement throughput
  versus connection count for `secantusd-pg` and PostgreSQL 16, with repeated
  trials, a median, and the run-to-run spread that says whether two rows differ.
- `tests/test_pg_concurrency_bench.py`, covering the summary arithmetic and the
  release-binary assumption a debug build would quietly invalidate.

#### Changed

- The storage-concurrency backlog entry now records what the lock guards, the
  measured scaling and CPU-per-operation figures, the confound that makes the
  high-connection rows untrustworthy on a four-performance-core box, and an
  explicit warning not to touch the lock as a concurrency fix.
- Six over-length `hint=` strings in `tasks.py` wrapped.

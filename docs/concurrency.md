# Concurrency: what scales, what doesn't

SecantusDB is a single-process embeddable MongoDB server. This page is
about what that means for **concurrent writers** — many client
connections issuing inserts/updates/deletes at the same time.

The short version: **don't expect write throughput to scale with the
number of concurrent writers**. The ceiling is in WiredTiger itself,
not in SecantusDB's Python layer above it. If your workload depends on
multi-writer scaling, run a real `mongod` instead.

## What scales fine

- **Concurrent reads.** Multiple `find` / `count` / `aggregate` calls
  against the same or different collections run in parallel under
  WiredTiger's MVCC. Reads don't block writes and don't block other
  reads.
- **Per-connection isolation.** Each TCP connection gets its own
  server thread and its own WiredTiger session. Sessions don't
  contend on each other for reads.
- **Single-writer throughput.** A single connection driving inserts
  via `insert_many` (batched) hits ~5,000 docs/s on commodity laptop
  hardware with logging on, or ~30,000+ docs/s with logging disabled
  (which trades crash durability for speed; not recommended for real
  workloads).

## What doesn't scale

Aggregate write throughput across multiple writer connections.
**Adding writer connections does not increase aggregate throughput
past N≈2 — and at N=4+ it can actively decrease it.**

We measured this carefully because the question kept coming up. The
benchmark and the data are at `bench/wt_poc/`; you can re-run it on
your hardware to confirm.

### The headline number

`bench/wt_poc/run.py` runs the same workload (50,000 row inserts,
each row ~1 KiB, partitioned across N writers writing to their own
table) through three paths:

| N writers | Pure-C + pthread (no Python) | Python + WT SWIG bindings |
|---|---|---|
| 1 | 276,449 rows/s (1.00×) | 116,578 rows/s (1.00×) |
| 2 | 340,106 rows/s (1.23×) | 87,010 rows/s (0.75×) |
| 4 | 352,731 rows/s (1.28×) | 67,660 rows/s (0.58×) |
| 8 | 285,146 rows/s (1.03×) | 58,751 rows/s (0.50×) |

The pure-C column is the theoretical best case: pthreads, no GIL, no
Python on the hot path, calling `libwiredtiger` directly. **Even
that** caps at ~1.3× of single-thread aggregate throughput at N=2 and
flatlines (or regresses) past that.

The bottleneck is at the WT C library level — B-tree page locks, log
write serialisation, cache eviction, internal scheduler. It's the
same library `mongod` uses, but `mongod` gets multi-writer scaling by
running a careful C++ scheduler above WT that takes advantage of
lower-level WT primitives (per-cursor concurrency hints, parallel
cursor batches, careful checkpoint coordination). SecantusDB doesn't
have that scheduler — and writing one isn't a SecantusDB project; it
would essentially be re-implementing `mongod`.

### Why disabling logging doesn't fix it

A natural follow-up: maybe the journal is the serialiser. We tested
that — same C benchmark, `log=(enabled=false)`:

| N writers | Pure-C + pthread, no log |
|---|---|
| 1 | 1,007,557 rows/s (1.00×) |
| 2 | 1,156,150 rows/s (1.15×) |
| 4 | 700,035 rows/s (**0.69×**) |
| 8 | 347,176 rows/s (**0.34×**) |

Single-thread is much faster (~4×) but multi-thread is *worse* —
collapses at N=4 and N=8. Disabling logging is a single-writer
optimisation that loses crash durability AND fails to deliver
concurrency.

### What this means for your workload

- **One connection doing batched writes** is the fastest configuration
  and what we recommend for tests / dev / single-process applications.
  `pymongo`'s `insert_many` with batch=100 is ~5,000 docs/s on
  commodity hardware with full durability.
- **Many connections doing concurrent writes** caps around the
  single-writer rate and may go *slower* if you push N high. Run a
  real `mongod` if your workload depends on this.
- **Many connections doing concurrent reads** scales fine. Reads use
  MVCC snapshots and don't contend.
- **Mixed read/write at moderate N** works as expected: writes
  serialise, reads run in parallel against an MVCC snapshot.

## Mitigations within SecantusDB

If you genuinely need higher single-process write throughput from
SecantusDB, the levers are:

1. **Batch larger.** `insert_many` with batch=100 is ~2× the
   throughput of `insert_one`. Going larger has diminishing returns.
2. **Reduce server-side work.** Drop indexes you don't need. Each
   index adds per-doc encode + WT cursor write.
3. **Disable the oplog if you don't need change streams.** Pass
   `replica_set_name=None` to `SecantusDBServer` (or run without
   `--auth` *and* without a replica-set advertisement). Halves
   per-write WT cursor traffic.
4. **`writeConcern: w:0`** for fire-and-forget writes — pymongo
   doesn't wait for the server's ack. Throughput climbs on the
   client side; server-side cost is unchanged.

## What we tried, what didn't work

The path we took to nail this down (preserved here so future
contributors don't re-walk it):

- **Lock-decomposition** (replace global `Storage._lock` with
  per-collection locks + tiny `_oplog_seq_lock`). Did clean up
  several internal correctness issues — see the
  ``tasks/wt-concurrency-plan.md`` writeup — but didn't move
  multi-writer scaling. Bottleneck wasn't the Python lock layer.
- **Profiling the insert hot path** (`bench/profile_insert.py`).
  Showed 50%+ of wall time was in WiredTiger's SWIG-generated Python
  bindings (`wiredtiger/packing.py`), not in our code. Suggested a
  Cython rebind would help.
- **The pure-C pthread benchmark** (`bench/wt_poc/`). Killed the
  Cython rebind hypothesis: even with no Python anywhere, WT itself
  doesn't scale past N≈2. The bindings are a constant overhead;
  removing them wouldn't change the multi-writer story.

The artefacts of all three exploration tracks are kept in the repo as
reproducible evidence. Re-run them when somebody asks "but what if we
just X?" and confirm the numbers haven't moved.

## Tracking

`tests/test_concurrency.py` is marked `xfail` (expected-fail) — it
encodes the goal "2 concurrent writers >= 0.7× of one" which the
storage backend cannot deliver. Useful as a regression *detector*: if
WiredTiger ever ships a higher-concurrency story upstream, that test
will unexpectedly pass and the surprise will surface in the test logs.

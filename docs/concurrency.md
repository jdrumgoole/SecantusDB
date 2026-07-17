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

### End-to-end: both servers vs mongod

Measured 2026-07-17 with the three-server harness
(`uv run python -m bench.concurrency --server all --writers 1,2,4,8`):
N writer processes, each streaming `insert_many` batches through
`pymongo` for 30 s against its own collection, all three servers on
on-disk WiredTiger.

```{raw} html
<style>
.dviz-wrap { --dv-mongo:#2a78d6; --dv-rust:#eb6834; --dv-py:#0891b2;
  --dv-ink:#334155; --dv-ink2:#64748b; --dv-grid:#e2e8f0; --dv-ref:#94a3b8; margin:14px 0; }
@media (prefers-color-scheme: dark) { body:not([data-theme="light"]) .dviz-wrap {
  --dv-mongo:#3987e5; --dv-rust:#d95926; --dv-py:#0891b2;
  --dv-ink:#cbd5e1; --dv-ink2:#94a3b8; --dv-grid:#1e293b; --dv-ref:#475569; } }
body[data-theme="dark"] .dviz-wrap {
  --dv-mongo:#3987e5; --dv-rust:#d95926; --dv-py:#0891b2;
  --dv-ink:#cbd5e1; --dv-ink2:#94a3b8; --dv-grid:#1e293b; --dv-ref:#475569; }
.dviz { width:100%; height:auto; display:block; }
.dv-lab { font:500 12.5px/1 sans-serif; fill:var(--dv-ink); }
.dv-val { font:600 11.5px/1 sans-serif; fill:var(--dv-ink); }
.dv-tick { font:500 11px/1 sans-serif; fill:var(--dv-ink2); }
.dv-grid { stroke:var(--dv-grid); stroke-width:1; }
.dv-ref { stroke:var(--dv-ref); stroke-width:1.5; stroke-dasharray:4 3; }
.dv-x { font-size:0.82em; opacity:0.75; }
.dv-legend { display:flex; gap:16px; flex-wrap:wrap; margin:6px 0 4px; font-size:0.85rem; color:var(--dv-ink2); }
.dv-legend .chip { display:inline-block; width:12px; height:12px; border-radius:3px; margin-right:6px; vertical-align:-1px; }
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-mongo)"></span>mongod</span><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 790 320" role="img" aria-label="Throughput scaling relative to each server single-writer rate" class="dviz"><line x1="56" y1="214" x2="668" y2="214" class="dv-ref"/><text x="48" y="218" text-anchor="end" class="dv-tick">1<tspan class="dv-x">x</tspan></text><line x1="56" y1="160" x2="668" y2="160" class="dv-grid"/><text x="48" y="164" text-anchor="end" class="dv-tick">2<tspan class="dv-x">x</tspan></text><line x1="56" y1="105" x2="668" y2="105" class="dv-grid"/><text x="48" y="109" text-anchor="end" class="dv-tick">3<tspan class="dv-x">x</tspan></text><line x1="56" y1="51" x2="668" y2="51" class="dv-grid"/><text x="48" y="55" text-anchor="end" class="dv-tick">4<tspan class="dv-x">x</tspan></text><text x="56" y="290" text-anchor="middle" class="dv-tick">1</text><text x="143" y="290" text-anchor="middle" class="dv-tick">2</text><text x="318" y="290" text-anchor="middle" class="dv-tick">4</text><text x="668" y="290" text-anchor="middle" class="dv-tick">8</text><text x="362" y="310" text-anchor="middle" class="dv-lab">concurrent writers</text><path d="M56.0,213.8 L143.4,168.2 L318.3,91.2 L668.0,44.1" fill="none" stroke="var(--dv-mongo)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-mongo)"><title>mongod — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="168.2" r="4.5" fill="var(--dv-mongo)"><title>mongod — 2 writers: 1.84x its single-writer rate</title></circle><circle cx="318.3" cy="91.2" r="4.5" fill="var(--dv-mongo)"><title>mongod — 4 writers: 3.26x its single-writer rate</title></circle><circle cx="668.0" cy="44.1" r="4.5" fill="var(--dv-mongo)"><title>mongod — 8 writers: 4.13x its single-writer rate</title></circle><path d="M56.0,213.8 L143.4,240.9 L318.3,240.3 L668.0,239.3" fill="none" stroke="var(--dv-rust)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-rust)"><title>Rust server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="240.9" r="4.5" fill="var(--dv-rust)"><title>Rust server — 2 writers: 0.50x its single-writer rate</title></circle><circle cx="318.3" cy="240.3" r="4.5" fill="var(--dv-rust)"><title>Rust server — 4 writers: 0.51x its single-writer rate</title></circle><circle cx="668.0" cy="239.3" r="4.5" fill="var(--dv-rust)"><title>Rust server — 8 writers: 0.53x its single-writer rate</title></circle><path d="M56.0,213.8 L143.4,251.2 L318.3,259.3 L668.0,258.8" fill="none" stroke="var(--dv-py)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-py)"><title>Python server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="251.2" r="4.5" fill="var(--dv-py)"><title>Python server — 2 writers: 0.31x its single-writer rate</title></circle><circle cx="318.3" cy="259.3" r="4.5" fill="var(--dv-py)"><title>Python server — 4 writers: 0.16x its single-writer rate</title></circle><circle cx="668.0" cy="258.8" r="4.5" fill="var(--dv-py)"><title>Python server — 8 writers: 0.17x its single-writer rate</title></circle><text x="674" y="48" class="dv-val" fill="var(--dv-mongo)">mongod 4.1<tspan class="dv-x">x</tspan></text><text x="674" y="240" class="dv-val" fill="var(--dv-rust)">Rust 0.5<tspan class="dv-x">x</tspan></text><text x="674" y="272" class="dv-val" fill="var(--dv-py)">Python 0.2<tspan class="dv-x">x</tspan></text></svg></div>
```

| N writers | Python server (docs/s) | Rust server (docs/s) | mongod (docs/s) |
|---|---:|---:|---:|
| 1 | 2,900 | 3,526 | 108,625 |
| 2 | 893 | 1,777 | 199,810 |
| 4 | 460 | 1,803 | 353,942 |
| 8 | 500 | 1,876 | 448,989 |

Three different shapes:

- **mongod scales** — 4.1× its own single-writer aggregate at N=8. That's
  the C++ scheduler above WT doing its job.
- **The Rust server holds flat** at roughly half its single-writer rate:
  its storage layer currently serialises writers behind one global mutex,
  so concurrency costs a constant coordination overhead and buys nothing —
  a measured improvement target, not a defect (writes stay correct and no
  client ever sees an error).
- **The Python server degrades** to ~0.2× under contention — the GIL plus
  the WT-binding ceiling measured above. The shared oplog-metadata row
  that used to make concurrent writers conflict at commit is no longer
  written on the hot path (neither per oplog emit nor per cluster-time
  mint — the latter ran on every driver heartbeat); the write conflicts
  WiredTiger still reports under saturation are retried with backoff,
  without a deadline, exactly like mongod's `writeConflictRetry` — a
  client never sees an error, generic or otherwise (both the
  swallowed-`InternalError` classification bug and the deadline that
  could surface `WriteConflict` on plain writes were found by this
  harness and fixed).

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

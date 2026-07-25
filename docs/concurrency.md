# Concurrency: what scales, what doesn't

SecantusDB is a single-process embeddable MongoDB server. This page is
about what that means for **concurrent writers** — many client
connections issuing inserts/updates/deletes at the same time.

The short version: write throughput scales **only up to a point**. The
Rust server now gets real scaling to about four concurrent writers
(peaking ~2.6× its single-writer rate) before a **WiredTiger** ceiling —
WAL serialisation, cache eviction, and checkpoint pressure inside one
embedded WT connection — pulls it back down; the Python server barely
scales at all (the GIL). The ceiling is in WiredTiger itself, not in a
SecantusDB lock. If your workload depends on write throughput that keeps
climbing past a handful of writers, run a real `mongod` instead.

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

Aggregate write throughput *without bound*. Both SecantusDB servers hit
a WiredTiger ceiling as writers pile up — the Rust server climbs to a
peak around four writers and then declines; a shared-table / large-row
workload (the pure-C `wt_poc` case below) tops out even earlier, around
N≈2. **Past the peak, adding writer connections actively decreases
aggregate throughput.**

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

Measured 2026-07-25 with the three-server harness
(`uv run python -m bench.concurrency --server all --writers 1,2,4,8`),
median of two quiesced runs: N writer processes, each streaming
`insert_many` batches through `pymongo` for 30 s against its own
collection, all three servers on on-disk WiredTiger.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-mongo)"></span>mongod</span><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 790 320" role="img" aria-label="Throughput scaling relative to each server single-writer rate" class="dviz"><line x1="56" y1="214" x2="668" y2="214" class="dv-ref"/><text x="48" y="218" text-anchor="end" class="dv-tick">1<tspan class="dv-x">x</tspan></text><line x1="56" y1="160" x2="668" y2="160" class="dv-grid"/><text x="48" y="164" text-anchor="end" class="dv-tick">2<tspan class="dv-x">x</tspan></text><line x1="56" y1="105" x2="668" y2="105" class="dv-grid"/><text x="48" y="109" text-anchor="end" class="dv-tick">3<tspan class="dv-x">x</tspan></text><line x1="56" y1="51" x2="668" y2="51" class="dv-grid"/><text x="48" y="55" text-anchor="end" class="dv-tick">4<tspan class="dv-x">x</tspan></text><text x="56" y="290" text-anchor="middle" class="dv-tick">1</text><text x="143" y="290" text-anchor="middle" class="dv-tick">2</text><text x="318" y="290" text-anchor="middle" class="dv-tick">4</text><text x="668" y="290" text-anchor="middle" class="dv-tick">8</text><text x="362" y="310" text-anchor="middle" class="dv-lab">concurrent writers</text><path d="M56.0,213.8 L143.4,165.5 L318.3,79.3 L668.0,20.1" fill="none" stroke="var(--dv-mongo)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-mongo)"><title>mongod — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="165.5" r="4.5" fill="var(--dv-mongo)"><title>mongod — 2 writers: 1.89x its single-writer rate</title></circle><circle cx="318.3" cy="79.3" r="4.5" fill="var(--dv-mongo)"><title>mongod — 4 writers: 3.48x its single-writer rate</title></circle><circle cx="668.0" cy="20.1" r="4.5" fill="var(--dv-mongo)"><title>mongod — 8 writers: 4.57x its single-writer rate</title></circle><path d="M56.0,213.8 L143.4,187.2 L318.3,125.4 L668.0,180.2" fill="none" stroke="var(--dv-rust)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-rust)"><title>Rust server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="187.2" r="4.5" fill="var(--dv-rust)"><title>Rust server — 2 writers: 1.49x its single-writer rate</title></circle><circle cx="318.3" cy="125.4" r="4.5" fill="var(--dv-rust)"><title>Rust server — 4 writers: 2.63x its single-writer rate</title></circle><circle cx="668.0" cy="180.2" r="4.5" fill="var(--dv-rust)"><title>Rust server — 8 writers: 1.62x its single-writer rate</title></circle><path d="M56.0,213.8 L143.4,233.3 L318.3,227.4 L668.0,237.7" fill="none" stroke="var(--dv-py)" stroke-width="2"/><circle cx="56.0" cy="213.8" r="4.5" fill="var(--dv-py)"><title>Python server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="233.3" r="4.5" fill="var(--dv-py)"><title>Python server — 2 writers: 0.64x its single-writer rate</title></circle><circle cx="318.3" cy="227.4" r="4.5" fill="var(--dv-py)"><title>Python server — 4 writers: 0.75x its single-writer rate</title></circle><circle cx="668.0" cy="237.7" r="4.5" fill="var(--dv-py)"><title>Python server — 8 writers: 0.56x its single-writer rate</title></circle><text x="674" y="24" class="dv-val" fill="var(--dv-mongo)">mongod 4.6<tspan class="dv-x">x</tspan></text><text x="674" y="184" class="dv-val" fill="var(--dv-rust)">Rust 1.6<tspan class="dv-x">x</tspan></text><text x="674" y="241" class="dv-val" fill="var(--dv-py)">Python 0.6<tspan class="dv-x">x</tspan></text></svg></div>
```

| N writers | Python server (docs/s) | Rust server (docs/s) | mongod (docs/s) |
|---|---:|---:|---:|
| 1 | 11,600 | 25,700 | 109,900 |
| 2 | 7,400 | 38,400 | 208,100 |
| 4 | 8,700 | 67,800 | 382,700 |
| 8 | 6,400 | 41,700 | 502,000 |

Three different shapes:

- **mongod scales** — 4.6× its own single-writer aggregate at N=8. That's
  the C++ scheduler above WT doing its job.
- **The Rust server now scales** — 1.5× at two writers, peaking **2.6× at
  four**, before easing back to 1.6× at eight. This is a change from the
  earlier "holds flat at ~0.5×" measurement: the per-collection write-lock
  split and the **RecordId keying** work (doc table and index entries keyed
  by a monotonic per-collection RecordId, cutting write amplification from
  four WT writes per document to three) between them turned constant
  coordination overhead into real low/moderate-concurrency scaling. The
  decline past four writers is **WiredTiger itself** — WAL log
  serialisation, cache eviction, and checkpoint pressure inside a single
  embedded WT connection — not a SecantusDB lock (the storage layer no
  longer serialises independent-collection writers). It is the same ceiling
  the pure-C `wt_poc` benchmark hits above; mongod clears it only by being a
  purpose-built WT host tuned for exactly this.
- **The Python server degrades** under contention — the GIL plus the
  WT-binding ceiling measured above hold it to ~0.6× of its single-writer
  rate. It degrades *gracefully*, though: the write conflicts WiredTiger
  reports under saturation are retried with backoff, without a deadline,
  exactly like mongod's `writeConflictRetry` — a client never sees an
  error, generic or otherwise (both the swallowed-`InternalError`
  classification bug and the deadline that could surface `WriteConflict` on
  plain writes were found by this harness and fixed).

Note too that **single-writer throughput itself jumped** since the
2026-07-17 baseline — the Rust server from ~3.5k to ~25.7k docs/s, the
Python server from ~2.9k to ~11.6k — the compounding effect of the raw-BSON
serving path and RecordId keying. mongod's single-writer rate is unchanged
(~110k), which is what validates the harness is measuring the same thing.

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

- **One connection doing batched writes** is a simple, fast
  configuration for tests / dev / single-process applications.
  `pymongo`'s `insert_many` with batch=100 hits ~25,000 docs/s on the
  Rust server and ~11,000 on the Python server on commodity hardware
  with full durability.
- **Many connections doing concurrent writes** scale on the Rust server
  up to a peak around four writers (~2.6× the single-writer rate), then
  decline as the WiredTiger ceiling bites; the Python server barely
  scales (the GIL). Run a real `mongod` if you need write throughput
  that keeps climbing past a handful of writers.
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
5. **Async oplog (Rust server, opt-in, experimental).** Set
   `SECANTUS_OPLOG_ASYNC=1` to move oplog writes off the writer's
   critical path onto a background drainer — ~1.4× multi-writer write
   throughput while keeping change streams (validated exactly-once
   under concurrency). Trade: the oplog is no longer atomic with the
   data, so a hard crash loses entries the drainer hadn't yet written
   (the data itself stays fully durable; a clean shutdown flushes the
   drainer). Bounded by `SECANTUS_OPLOG_ASYNC_CAP_BYTES` (default
   128 MB). Default off; see the Rust-server notes for the ceiling
   analysis (a parallel drainer pool does *not* help — WiredTiger's
   aggregate write throughput is the shared limit).
6. **WiredTiger config tuning (Rust server / daemon).**
   `SECANTUS_WT_CONFIG_EXTRA` appends raw WT connection config
   (last-key-wins). Log pre-allocation
   (`log=(file_max=512MB,prealloc=true)`) lifts the write ceiling
   ~8%; a larger `cache_size` lifts read-modify-write (update/delete)
   throughput notably. Each trades disk or memory; measured gains are
   modest (~10%).

The honest ceiling: for oplog-backed multi-writer throughput the limit
is WiredTiger's own aggregate write rate on a single embedded process —
the levers above buy ~10–40%, but sustained multi-writer scaling beyond
that means running a real `mongod` (or dropping the oplog entirely).

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

`tests/test_concurrency.py` (which drives the **Python** server) is
marked `xfail` (expected-fail) — it encodes the goal "2 concurrent
writers >= 0.7× of one", which the Python server's GIL-bound path
cannot deliver (it measures ~0.64× at two writers). The **Rust** server
now clears that bar comfortably (~1.5× at two writers, peaking ~2.6× at
four — see the chart above); the xfail tracks the Python server
specifically, and stays a useful regression *detector* for it.

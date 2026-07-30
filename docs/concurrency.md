# Concurrency: what scales, what doesn't

SecantusDB is a single-process embeddable MongoDB server. This page is
about what that means for **concurrent writers** — many client
connections issuing inserts/updates/deletes at the same time.

The short version: the Rust server's fully-durable write path now
scales **monotonically** — ~1.4× at two writers, ~2.5× at four, ~2.6×
at eight — with no cliff (the earlier peak-then-collapse shape was
oplog-prune churn on the write path plus over-sharded oplog btrees;
both fixed). The remaining gap to mongod's ~4.5× is a **WiredTiger**
ceiling — cache eviction and checkpoint pressure inside one embedded WT
connection — not a SecantusDB lock. The opt-in async + non-logged
oplog stack (mitigations 5–6 below) starts from a ~1.6× higher
single-writer base and reaches the highest absolute throughput at
every writer count, at the cost of a checkpoint-durable-only oplog
tail. The Python server barely scales at all (the GIL). If your
workload depends on write throughput that keeps climbing past eight
writers, run a real `mongod` instead.

## What scales fine

- **Concurrent reads.** Multiple `find` / `count` / `aggregate` calls
  against the same or different collections run in parallel under
  WiredTiger's MVCC. Reads don't block writes and don't block other
  reads.
- **Per-connection isolation.** Each TCP connection gets its own
  server thread and its own WiredTiger session. Sessions don't
  contend on each other for reads.
- **Single-writer throughput.** A single connection driving batched
  `insert_many` of 8 KiB documents sustains ~31,700 docs/s on the Rust
  server and ~11,800 docs/s on the Python server (fully durable,
  WAL-logged, on commodity laptop hardware; 2026-07-30 baseline).

## What doesn't scale

Aggregate write throughput *without bound*. Both SecantusDB servers hit
a WiredTiger ceiling as writers pile up — the Rust server's scaling
flattens between four and eight writers (~2.5× → ~2.6×, still
monotonic); a shared-table / large-row workload (the pure-C `wt_poc`
case below) tops out much earlier, around N≈2, and actively regresses
past its peak.

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

Measured 2026-07-30 with the three-server harness
(`uv run python -m bench.concurrency --server all --writers 1,2,4,8`),
medians of three interleaved quiesced runs: N writer processes, each
streaming `insert_many` batches through `pymongo` against its own
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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-mongo)"></span>mongod</span><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span><span><span class="chip" style="background:transparent;border:2px dashed var(--dv-rust);box-sizing:border-box"></span>Rust server — async stack</span></div><svg viewBox="0 0 790 320" role="img" aria-label="Throughput scaling relative to each server single-writer rate" class="dviz"><line x1="56" y1="214" x2="668" y2="214" class="dv-ref"/><text x="48" y="218" text-anchor="end" class="dv-tick">1<tspan class="dv-x">x</tspan></text><line x1="56" y1="160" x2="668" y2="160" class="dv-grid"/><text x="48" y="164" text-anchor="end" class="dv-tick">2<tspan class="dv-x">x</tspan></text><line x1="56" y1="105" x2="668" y2="105" class="dv-grid"/><text x="48" y="109" text-anchor="end" class="dv-tick">3<tspan class="dv-x">x</tspan></text><line x1="56" y1="51" x2="668" y2="51" class="dv-grid"/><text x="48" y="55" text-anchor="end" class="dv-tick">4<tspan class="dv-x">x</tspan></text><text x="56" y="290" text-anchor="middle" class="dv-tick">1</text><text x="143" y="290" text-anchor="middle" class="dv-tick">2</text><text x="318" y="290" text-anchor="middle" class="dv-tick">4</text><text x="668" y="290" text-anchor="middle" class="dv-tick">8</text><text x="362" y="310" text-anchor="middle" class="dv-lab">concurrent writers</text><path d="M56.0,214.0 L143.4,166.2 L318.3,84.6 L668.0,26.0" fill="none" stroke="var(--dv-mongo)" stroke-width="2"/><circle cx="56.0" cy="214.0" r="4.5" fill="var(--dv-mongo)"><title>mongod — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="166.2" r="4.5" fill="var(--dv-mongo)"><title>mongod — 2 writers: 1.88x its single-writer rate</title></circle><circle cx="318.3" cy="84.6" r="4.5" fill="var(--dv-mongo)"><title>mongod — 4 writers: 3.38x its single-writer rate</title></circle><circle cx="668.0" cy="26.0" r="4.5" fill="var(--dv-mongo)"><title>mongod — 8 writers: 4.46x its single-writer rate</title></circle><path d="M56.0,214.0 L143.4,191.5 L318.3,131.9 L668.0,128.1" fill="none" stroke="var(--dv-rust)" stroke-width="2"/><circle cx="56.0" cy="214.0" r="4.5" fill="var(--dv-rust)"><title>Rust server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="191.5" r="4.5" fill="var(--dv-rust)"><title>Rust server — 2 writers: 1.42x its single-writer rate</title></circle><circle cx="318.3" cy="131.9" r="4.5" fill="var(--dv-rust)"><title>Rust server — 4 writers: 2.51x its single-writer rate</title></circle><circle cx="668.0" cy="128.1" r="4.5" fill="var(--dv-rust)"><title>Rust server — 8 writers: 2.58x its single-writer rate</title></circle><path d="M56.0,214.0 L143.4,226.7 L318.3,220.7 L668.0,233.7" fill="none" stroke="var(--dv-py)" stroke-width="2"/><circle cx="56.0" cy="214.0" r="4.5" fill="var(--dv-py)"><title>Python server — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="226.7" r="4.5" fill="var(--dv-py)"><title>Python server — 2 writers: 0.77x its single-writer rate</title></circle><circle cx="318.3" cy="220.7" r="4.5" fill="var(--dv-py)"><title>Python server — 4 writers: 0.88x its single-writer rate</title></circle><circle cx="668.0" cy="233.7" r="4.5" fill="var(--dv-py)"><title>Python server — 8 writers: 0.64x its single-writer rate</title></circle><path d="M56.0,214.0 L143.4,194.5 L318.3,156.9 L668.0,138.6" fill="none" stroke="var(--dv-rust)" stroke-width="2" stroke-dasharray="6 4"/><circle cx="56.0" cy="214.0" r="4.5" fill="var(--dv-rust)"><title>Rust server (async + non-logged oplog) — 1 writer: 1.00x its single-writer rate</title></circle><circle cx="143.4" cy="194.5" r="4.5" fill="var(--dv-rust)"><title>Rust server (async + non-logged oplog) — 2 writers: 1.36x its single-writer rate</title></circle><circle cx="318.3" cy="156.9" r="4.5" fill="var(--dv-rust)"><title>Rust server (async + non-logged oplog) — 4 writers: 2.05x its single-writer rate</title></circle><circle cx="668.0" cy="138.6" r="4.5" fill="var(--dv-rust)"><title>Rust server (async + non-logged oplog) — 8 writers: 2.39x its single-writer rate</title></circle><text x="674" y="30" class="dv-val" fill="var(--dv-mongo)">mongod 4.5<tspan class="dv-x">x</tspan></text><text x="674" y="150" class="dv-val" fill="var(--dv-rust)">async 2.4<tspan class="dv-x">x</tspan></text><text x="674" y="124" class="dv-val" fill="var(--dv-rust)">Rust 2.6<tspan class="dv-x">x</tspan></text><text x="674" y="238" class="dv-val" fill="var(--dv-py)">Python 0.6<tspan class="dv-x">x</tspan></text></svg></div>
```

| N writers | Python server (docs/s) | Rust server (docs/s) | Rust — async stack (docs/s) | mongod (docs/s) |
|---|---:|---:|---:|---:|
| 1 | 11,800 | 31,700 | 49,300 | 108,400 |
| 2 | 9,000 | 44,800 | 67,100 | 203,900 |
| 4 | 10,300 | 79,500 | 101,200 | 366,800 |
| 8 | 7,500 | 81,800 | 117,800 | 483,600 |

(The async-stack column is the opt-in `SECANTUS_OPLOG_ASYNC=1` +
`SECANTUS_OPLOG_NONLOGGED=1` configuration, mitigations 5–6 below.
Scaling in the chart is relative to each series' own single-writer rate.)

Three different shapes:

- **mongod scales** — 4.5× its own single-writer aggregate at N=8. That's
  the C++ scheduler above WT doing its job.
- **The Rust server now scales monotonically** — 1.4× at two writers,
  2.5× at four, **2.6× at eight**, with no cliff. The earlier
  peak-then-collapse shape (2.6× at four easing back to 1.6× at eight)
  was diagnosed by profiling and eliminated in two steps: the
  opportunistic **oplog prune** was consuming ~36% of the sustained
  write path (every sweep re-read the full value of every doomed row;
  it now scans keys only), and the oplog's 16-way shard split had
  outlived the append hotspot it was built for (writes now route
  across two append-tuned btrees). Together with the per-collection
  write-lock split and the **RecordId keying** work (write
  amplification cut from four WT writes per document to three), the
  fully-durable default now holds ~half of mongod's scaling ratio. The
  remaining flattening between four and eight writers is **WiredTiger
  itself** — cache eviction and checkpoint pressure inside a single
  embedded WT connection — not a SecantusDB lock.
- **The async stack delivers the highest absolute throughput** (dashed
  line) — the opt-in `SECANTUS_OPLOG_ASYNC=1` +
  `SECANTUS_OPLOG_NONLOGGED=1` configuration (mitigations 5–6 below)
  moves the oplog write off the writers' critical path and out of the
  WAL: 2.4× scaling from a ~1.6× higher single-writer base (49.3k vs
  31.7k docs/s), reaching ~118k docs/s at eight writers — ~1.4× the
  default's 82k. The trade is that a hard crash loses the oplog tail
  since the last checkpoint (data stays fully durable; change streams
  remain exactly-once).
- **The Python server degrades** under contention — the GIL plus the
  WT-binding ceiling measured above hold it to ~0.6× of its single-writer
  rate. It degrades *gracefully*, though: the write conflicts WiredTiger
  reports under saturation are retried with backoff, without a deadline,
  exactly like mongod's `writeConflictRetry` — a client never sees an
  error, generic or otherwise (both the swallowed-`InternalError`
  classification bug and the deadline that could surface `WriteConflict` on
  plain writes were found by this harness and fixed).

Note too that **single-writer throughput itself keeps climbing** — the
Rust server from ~3.5k docs/s at the 2026-07-17 baseline to ~25.7k
post-RecordId to **~31.7k** now (the prune fix and the append-tuned
defaults), the Python server from ~2.9k to ~11.8k. mongod's
single-writer rate is unchanged (~108k), which is what validates the
harness is measuring the same thing.

### mongod pays for an oplog too

One asymmetry in the chart above: the mongod it benchmarks is
**standalone** — no replica set, so no oplog at all — while SecantusDB
always maintains one (change streams need it). Charging mongod for the
same feature changes the picture. `bench/mongod_replset_ab.py` runs the
identical workload against a single-node replica set (medians of 3
interleaved reps):

| mongod configuration | 1 writer | 8 writers |
|---|---:|---:|
| standalone (no oplog) | 113.2k docs/s | 503k docs/s |
| replica set, explicit `w:1` | 84.0k docs/s | 305k docs/s |
| replica set, default write concern | 11.8k docs/s | 68.6k docs/s |

The oplog double-write costs mongod −26% (1 writer) / −39% (8 writers) —
the same structural tax SecantusDB pays, at about half the rate (its
timestamp-slot oplog admits concurrent appends without the shared-append
serialisation we shard around). The bigger surprise is the default-config
row: since MongoDB 5.0 the implicit write concern is `w:majority`, and on
a one-node set a majority ack waits for a journal fsync — a ÷7 cliff that
dwarfs the oplog itself. So a single-node replica-set mongod *as people
actually run it for change streams* writes at 11.8k / 68.6k docs/s on
this hardware — slower than the Rust server's async + non-logged stack
(49.5k / ~125k), though with stronger per-write durability (its
acknowledged writes survive a hard crash; ours trade that per-ack fsync
away everywhere). At equal write-concern semantics mongod's raw ingest
path is still ~3× faster per writer; that residual is its C++ ingest
machinery, not the oplog.

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
  scales (the GIL). The opt-in async + non-logged oplog stack
  (mitigations 5–6) removes the cliff — monotonic to ~2.4× at eight
  writers, ~118k docs/s aggregate — if a checkpoint-durable oplog tail is
  acceptable. Run a real `mongod` if you need fully durable write
  throughput that keeps climbing past a handful of writers.
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
   critical path onto a background drainer — ~1.6× multi-writer write
   throughput while keeping change streams (validated exactly-once
   under concurrency). Trade: the oplog is no longer atomic with the
   data, so a hard crash loses entries the drainer hadn't yet written
   (the data itself stays fully durable; a clean shutdown flushes the
   drainer). Bounded by `SECANTUS_OPLOG_ASYNC_CAP_BYTES` (default
   128 MB). The drainer coalesces queued batches into one WiredTiger
   transaction (`SECANTUS_OPLOG_ASYNC_COALESCE=0` disables). Default
   off.
6. **Non-logged oplog tables (Rust server, opt-in).** Set
   `SECANTUS_OPLOG_NONLOGGED=1` (at first open of a fresh store) to
   create the oplog + pre-image tables with WAL logging disabled: the
   oplog becomes checkpoint-durable only — a hard crash loses the
   oplog tail since the last checkpoint (change-stream resume / PITR
   granularity), while the data tables stay fully logged and durable;
   a clean shutdown checkpoints a complete oplog. Alone it buys little
   (~4% in sync mode), but **stacked with the async oplog it removes
   the oplog's WAL volume from the writers' path entirely: ~2.2× the
   default 8-writer throughput** (measured 56k → 125k docs/s of 8 KiB
   documents, against a ~191k no-oplog ceiling), and ~1.9× a single
   writer. Change streams remain exactly-once under the stack.
7. **WiredTiger config tuning (Rust server / daemon).**
   `SECANTUS_WT_CONFIG_EXTRA` appends raw WT connection config
   (last-key-wins). A larger `cache_size` is the strongest single
   knob under sustained writes (+26% at eight writers in the
   Finding-13 sweep) — the daemon and the Python `RustServer` handle
   now default to a 4G cache *cap* (WiredTiger fills it lazily, so
   idle test servers stay small; `--cache-size` / `cache_size=`
   overrides). Two cautions from the same sweep, post prune-fix:
   log pre-allocation (`prealloc=true`) now *hurts* at eight writers
   (−8%; the earlier +8% predates the prune fix), and **never turn
   oplog block compression off** — throughput craters to ~19% of the
   ceiling, because bigger uncompressed pages mean more eviction IO
   and IO volume, not CPU, is the constraint.

The defaults already carry the measured winners: oplog writes route
across two shard tables (sixteen existed to spread an append hotspot
that the RecordId + prune work eliminated; the read side still scans
all sixteen, so any store stays readable), and the oplog/preimage
btrees are created append-tuned (`split_pct=100,leaf_page_max=128KB`).
With those defaults a fully-durable eight-writer load sustains ~54% of
the no-oplog ceiling (~103k docs/s of 8 KiB documents on the reference
box, vs ~43%/75k before).

The honest ceiling: for a fully-durable, WAL-logged oplog the limit is
WiredTiger's aggregate write rate on a single embedded process. The
async + non-logged stack (levers 5+6) trades crash-durability of the
oplog *tail* for the last stretch toward the no-oplog ceiling; past
that, sustained multi-writer scaling means running a real `mongod`
(or dropping the oplog entirely).

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

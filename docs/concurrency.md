# Concurrency: what scales, what doesn't

SecantusDB is a single-process embeddable MongoDB server. This page is
about what that means for **concurrent writers** — many client
connections issuing inserts/updates/deletes at the same time.

The short version, measured 2026-11-18 with **both servers as a one-member
replica set whose default writes wait for the journal**: the Rust server's
default write path scales monotonically, 1.5× at two writers, 2.1× at four
and 2.6× at eight (10.2k docs/s), and has no cliff. `mongod` 8.0.38 scales
further, to 3.4× (19.7k docs/s). It starts 1.5× ahead with one writer and
is 1.9× ahead at eight. The opt-in async + non-logged oplog stack
(mitigations 5–6 below) is not charted: the run of 2026-11-18 used it on a
store whose data tables were not logged, which is not the documented stack
and which lost acknowledged writes in a crash (RM-731, now refused). It is
shown again once re-measured on logged data tables. If your
workload depends on write throughput that keeps climbing past eight
writers, run a real `mongod` instead.

:::{note}
**Not comparable to figures published before 2026-08-22.** The sweep used to
share one store across every writer count, so each row measured a different
database — with 8 KiB documents the eight-writer row wrote into a tree holding
everything the earlier rows left behind. That biased scaling *downwards*, and
on a smaller disk it eventually exhausted space outright and took WiredTiger's
ENOSPC panic mid-sweep.

Each row now gets a fresh store. Every engine improved as a result, most at
high writer counts where the accumulation was worst — including `mongod`, which
is unchanged code and gained 14% at eight writers.

**Measured on a dedicated cloud instance (8 vCPU) against mongod 8.0.29**, as
part of cutting `secantusdb-v0.5.3-beta.163` — so these are the figures for the
build you can actually download. Absolute throughput is lower than the earlier
workstation-measured numbers because the cores are slower; the **scaling
ratios**, which is what this page reports, are if anything better. `mongod` is
the control: it scales 5.15× here against 4.65× on that workstation, which
confirms eight writers on eight vCPUs is not core-starved.
:::

## What scales fine

- **Concurrent reads.** Multiple `find` / `count` / `aggregate` calls
  against the same or different collections run in parallel under
  WiredTiger's MVCC. Reads don't block writes and don't block other
  reads.
- **Per-connection isolation.** Each TCP connection gets its own
  server thread and its own WiredTiger session. Sessions don't
  contend on each other for reads.
- **Single-writer throughput.** A single connection driving batched
  `insert_many` of 8 KiB documents sustains ~12,687 docs/s on the Rust
  server (fully durable,
  WAL-logged, on an 8-vCPU cloud instance; 2026-08-26 baseline).

## What doesn't scale

Aggregate write throughput *without bound*. Both SecantusDB servers hit
a WiredTiger ceiling as writers pile up — the Rust server's scaling
flattens between four and eight writers (2.1× → 2.6×, still
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

### End-to-end: the Rust server vs mongod

Measured 2026-07-31 with the concurrency harness
(`uv run python -m bench.concurrency --server all --writers 1,2,4,8
--runs 3`), medians of three interleaved quiesced runs: N writer
processes, each streaming `insert_many` batches through `pymongo`
against its own collection, all servers on on-disk WiredTiger. The
chart and table below are regenerated from those measurements on every
release (`invoke concurrency-refresh`); the committed numbers live in
`bench/results/concurrency.json`.

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
</style>
<!-- concurrency-viz:begin -->
<div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-mongo)"></span>mongod</span><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span></div><svg viewBox="0 0 790 320" role="img" aria-label="Write throughput in documents per second as concurrent writers are added" class="dviz"><text x="48" y="12" text-anchor="end" class="dv-tick">docs/s</text><line x1="56" y1="270" x2="668" y2="270" class="dv-grid"/><text x="48" y="274" text-anchor="end" class="dv-tick">0</text><line x1="56" y1="212" x2="668" y2="212" class="dv-grid"/><text x="48" y="216" text-anchor="end" class="dv-tick">5k</text><line x1="56" y1="153" x2="668" y2="153" class="dv-grid"/><text x="48" y="157" text-anchor="end" class="dv-tick">10k</text><line x1="56" y1="94" x2="668" y2="94" class="dv-grid"/><text x="48" y="98" text-anchor="end" class="dv-tick">15k</text><line x1="56" y1="36" x2="668" y2="36" class="dv-grid"/><text x="48" y="40" text-anchor="end" class="dv-tick">20k</text><text x="56" y="290" text-anchor="middle" class="dv-tick">1</text><text x="143" y="290" text-anchor="middle" class="dv-tick">2</text><text x="318" y="290" text-anchor="middle" class="dv-tick">4</text><text x="668" y="290" text-anchor="middle" class="dv-tick">8</text><text x="362" y="310" text-anchor="middle" class="dv-lab">concurrent writers</text><path d="M56.0,201.2 L143.4,155.8 L318.3,90.0 L668.0,39.4" fill="none" stroke="var(--dv-mongo)" stroke-width="2"/><circle cx="56.0" cy="201.2" r="4.5" fill="var(--dv-mongo)"><title>mongod — 1 writer: 5,900 docs/s</title></circle><circle cx="143.4" cy="155.8" r="4.5" fill="var(--dv-mongo)"><title>mongod — 2 writers: 9,800 docs/s</title></circle><circle cx="318.3" cy="90.0" r="4.5" fill="var(--dv-mongo)"><title>mongod — 4 writers: 15,400 docs/s</title></circle><circle cx="668.0" cy="39.4" r="4.5" fill="var(--dv-mongo)"><title>mongod — 8 writers: 19,700 docs/s</title></circle><path d="M56.0,223.6 L143.4,198.8 L318.3,172.3 L668.0,150.7" fill="none" stroke="var(--dv-rust)" stroke-width="2"/><circle cx="56.0" cy="223.6" r="4.5" fill="var(--dv-rust)"><title>Rust server — 1 writer: 4,000 docs/s</title></circle><circle cx="143.4" cy="198.8" r="4.5" fill="var(--dv-rust)"><title>Rust server — 2 writers: 6,100 docs/s</title></circle><circle cx="318.3" cy="172.3" r="4.5" fill="var(--dv-rust)"><title>Rust server — 4 writers: 8,300 docs/s</title></circle><circle cx="668.0" cy="150.7" r="4.5" fill="var(--dv-rust)"><title>Rust server — 8 writers: 10,200 docs/s</title></circle><text x="674" y="43" class="dv-val" fill="var(--dv-mongo)">mongod 19.7k</text><text x="674" y="155" class="dv-val" fill="var(--dv-rust)">Rust 10.2k</text></svg></div>
<!-- concurrency-viz:end -->
```

<!-- concurrency-table:begin -->
| N writers | Rust server (docs/s) | mongod (docs/s) |
|---|---:|---:|
| 1 | 4,000 | 5,900 |
| 2 | 6,100 | 9,800 |
| 4 | 8,300 | 15,400 |
| 8 | 10,200 | 19,700 |
<!-- concurrency-table:end -->

(The chart is each server's aggregate write throughput in documents per
second. The async + non-logged oplog stack, mitigations 5–6 below, is left out until
it is re-measured on logged data tables: see "The async stack" below.)

Three different shapes, from one droplet run of three interleaved passes
(2026-11-18, DigitalOcean `s-8vcpu-16gb`, medians shown):

- **mongod scales furthest.** It reaches 1.7× at two writers, 2.6× at four
  and 3.4× at eight, relative to its own single-writer rate. That is the C++
  scheduler above WiredTiger doing its job.
- **The Rust server scales monotonically.** It reaches 1.5×, 2.1× and 2.6×,
  with no cliff in any pass. The earlier peak-then-collapse shape (2.6× at
  four, easing back to 1.6× at eight) was found by profiling and removed in
  two steps:
  - The opportunistic **oplog prune** had been taking ~36% of the sustained
    write path. Every sweep re-read the full value of every doomed row; it
    now scans keys only.
  - The oplog's 16-way shard split had outlived the append hotspot it was
    built for. Writes now route across two append-tuned btrees.

  The flattening that remains between four and eight writers is
  **WiredTiger itself**: cache eviction and checkpoint pressure inside one
  embedded WiredTiger connection, not a SecantusDB lock.
- **The async stack is not shown.** The 2026-11-18 run started this opt-in
  configuration (mitigations 5–6 below) with `--oplog-async
  --oplog-nonlogged` on the default store, whose data tables are not logged.
  There the drainer is not used, and with the oplog out of WiredTiger's log
  too, nothing is logged at all: a SIGKILL after 2,000 `j: true` inserts left
  none of them (measured 2026-11-20, RM-731). The server now refuses that
  combination, `bench/concurrency.py` runs the stack with `--data-logged` as
  documented, and it returns to the chart once re-measured that way.

**Passes are noisy on a shared-CPU droplet.** At eight writers the three
passes gave 9.8k–14.6k docs/s for the Rust server and 16.2k–29.1k for
`mongod`, so read the shapes, not the last digit. Absolute rates depend on the
machine; compare them only within one run.

Until 2026-11-18 this chart compared against a *standalone* `mongod`, which
does not wait for the journal on a default write. Its figures (for example
41.5k docs/s for the Rust server and 127.7k for `mongod` at eight writers)
measured weaker durability on the `mongod` side and are not comparable with
these.

### mongod pays for an oplog too

The chart above now runs `mongod` as a single-node replica set. That mode
keeps an oplog, as SecantusDB always does because change streams need one.
This older measurement (workstation, 2026-10) shows what that choice costs
`mongod`. `bench/mongod_replset_ab.py` runs the
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
  configuration for dev / single-process applications.
  `pymongo`'s `insert_many` with batch=100 hits ~25,000 docs/s on the
  Rust server on commodity hardware
  with full durability.
- **Many connections doing concurrent writes** scale monotonically on
  the Rust server — ~2.7× the single-writer rate at eight writers
  (~96k docs/s aggregate) with no cliff, fully durable. The opt-in async + non-logged oplog
  stack (mitigations 5–6), on a store whose data tables are logged
  (`--data-logged`), buys another ~1.2× (~119k docs/s at eight
  writers) if a checkpoint-durable oplog tail is acceptable. Run a
  real `mongod` if you need write throughput that keeps climbing
  steeply past a handful of writers.
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
3. **Disable the oplog if you don't need change streams.** Start
   `secantusd-mdb` with `SECANTUS_DISABLE_OPLOG=1` set (change streams
   then have nothing to read). Halves
   per-write WT cursor traffic.
4. **`writeConcern: w:0`** for fire-and-forget writes — pymongo
   doesn't wait for the server's ack. Throughput climbs on the
   client side; server-side cost is unchanged.
5. **Async oplog (Rust server, opt-in).** `RustServer(oplog_async=True)`,
   `secantusd-mdb --oplog-async` (or `[storage] oplog_async = true` in
   `secantusd.toml`), or `SECANTUS_OPLOG_ASYNC=1` — the option wins over
   the env var for that store. Takes effect only on a store whose data
   tables are logged (`--data-logged`); on the default unlogged ones each
   write carries its oplog timestamp, so the drainer is not used and the
   server says so at startup. Moves oplog writes off the writer's
   critical path onto a background drainer — ~1.6× multi-writer write
   throughput while keeping change streams (validated exactly-once
   under concurrency; a fresh change stream opens only after the
   drainer has caught the acknowledged tail, so it never surfaces
   pre-open events; multi-document transactions buffer their entries
   and hand them to the drainer only at commit, so a rolled-back
   transaction never surfaces a change event). Trade: the oplog is no longer atomic with the
   data, so a hard crash loses entries the drainer hadn't yet written
   (the data itself stays fully durable; a clean shutdown flushes the
   drainer). Bounded by `SECANTUS_OPLOG_ASYNC_CAP_BYTES` (default
   128 MB). The drainer coalesces queued batches into one WiredTiger
   transaction (`SECANTUS_OPLOG_ASYNC_COALESCE=0` disables). Default
   off. (The async figures in the table above were measured before the
   async path gained the same opportunistic oplog prune the sync path
   has always had; the prune's retention enforcement costs ~5% at
   eight writers, so read the async column as slightly optimistic
   until the next release re-baseline.)
6. **Non-logged oplog tables (Rust server, opt-in).**
   `RustServer(oplog_nonlogged=True)`, `secantusd-mdb --oplog-nonlogged`
   (or the TOML key), or `SECANTUS_OPLOG_NONLOGGED=1` — applied at first
   open of a fresh store (the table config is create-time-sticky) — to
   create the oplog + pre-image tables with WAL logging disabled: the
   oplog becomes checkpoint-durable only — a hard crash loses the
   oplog tail since the last checkpoint (change-stream resume / PITR
   granularity), while the data tables stay fully logged and durable;
   a clean shutdown checkpoints a complete oplog. **Only on a store whose
   data tables are logged** (`--data-logged` / `data_nonlogged=False`):
   on the default unlogged data tables the data is recovered by replaying
   the oplog, so a non-logged oplog would leave nothing in the log and a
   crash would lose every write since the last checkpoint. The server
   refuses that combination at open, naming `--data-logged` (RM-731).
   Alone it buys little
   (~4% in sync mode), but **stacked with the async oplog it removes
   the oplog's WAL volume from the writers' path entirely: ~2.2× the
   default 8-writer throughput** (measured 56k → 125k docs/s of 8 KiB
   documents, against a ~191k no-oplog ceiling), and ~1.9× a single
   writer. Change streams remain exactly-once under the stack.
7. **WiredTiger config tuning (Rust server / daemon).**
   `SECANTUS_WT_CONFIG_EXTRA` appends raw WT connection config
   (last-key-wins). A larger `cache_size` is the strongest single
   knob under sustained writes (+26% at eight writers in the
   Finding-13 sweep) — the daemon, the Python `RustServer` handle,
   and the embedded Rust `Storage` library all default to a 4G cache
   *cap* (WiredTiger fills it lazily, so idle test servers stay
   small; `--cache-size` / `cache_size=` overrides). Two cautions from the same sweep, post prune-fix:
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
With those defaults a fully-durable eight-writer load sustains ~half
of the no-oplog ceiling (~96k docs/s of 8 KiB documents on the
reference box, vs ~43%/75k before the prune fix and these defaults).

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

The concurrency chart above is regenerated by `invoke do-perf` and
`bench/concurrency_chart.py`; compare a new run with the one published
here to catch a scaling regression.

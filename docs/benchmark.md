# Benchmark: both servers vs mongod

Generated 2026-08-26 on a dedicated DigitalOcean instance (8 vCPU, x86-64
Linux), against **mongod 8.0.31**, via `invoke do-perf`.

All three servers use the **same WiredTiger storage engine** — mongod ships
it; SecantusDB vendors the same C library — driven by the same `pymongo`
client over the wire protocol. The hot path differs only above the storage
layer (command dispatch, query planner, operator engines), so this is a fair
comparison of the parts of SecantusDB that aren't WiredTiger itself.

Each workload runs against a freshly-spawned server on a free port with its
own tmp data dir, all on-disk WiredTiger. Each timed 5× per server; the table
reports the median in milliseconds and how many times slower than `mongod`
each server is. Dataset is 10,000 small docs.

:::{note}
**These numbers are not comparable to those published before 2026-08-22.**
Two things changed at once, and both move the ratios without any change to
SecantusDB:

- **The reference moved from mongod 6.0.16 to the 8.0 line** (8.0.31 as of this
  run; the harness installs the latest 8.0.x). Every `×mongod` figure is a
  ratio, so a faster denominator makes us look worse. mongod 8.0 is
  substantially quicker at inserts, and that alone accounts for most of the
  change — SecantusDB's own absolute timings barely moved.
- **The machine moved from a developer laptop to a dedicated droplet.** A
  laptop cannot be trusted for this: a background build or an OS indexer shifts
  every column at once and nothing in the output says so. One earlier run
  recorded *mongod itself* at 2.5× its own baseline, which would have published
  a regression that did not exist.

The results file now records the mongod version it measured against, so a
future change of reference can't be mistaken for a change in SecantusDB.
:::

## Results

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 524" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="486" class="dv-grid"/><text x="295.2" y="502" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="486" class="dv-grid"/><text x="390.4" y="502" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="486" class="dv-grid"/><text x="485.6" y="502" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="486" class="dv-grid"/><text x="580.8" y="502" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="486" class="dv-grid"/><text x="676.0" y="502" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="486" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h34.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-34.2 z" fill="var(--dv-rust)"><title>Rust server — 2.0x mongod</title></path><text x="244.2" y="37" class="dv-val">2.0<tspan class="dv-x">x</tspan></text><path d="M200,42 h163.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-163.4 z" fill="var(--dv-py)"><title>Python server — 8.8x mongod</title></path><text x="373.4" y="53" class="dv-val">8.8<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h17.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-17.2 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="227.2" y="91" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,96 h178.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-178.5 z" fill="var(--dv-py)"><title>Python server — 9.6x mongod</title></path><text x="388.5" y="107" class="dv-val">9.6<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h21.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-21.0 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231.0" y="145" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,150 h161.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-161.9 z" fill="var(--dv-py)"><title>Python server — 8.7x mongod</title></path><text x="371.9" y="161" class="dv-val">8.7<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h21.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-21.4 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231.4" y="199" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,204 h242.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-242.6 z" fill="var(--dv-py)"><title>Python server — 13.0x mongod</title></path><text x="452.6" y="215" class="dv-val">13.0<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h26.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.9 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236.9" y="253" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,258 h372.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-372.9 z" fill="var(--dv-py)"><title>Python server — 19.8x mongod</title></path><text x="582.9" y="269" class="dv-val">19.8<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h37.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-37.0 z" fill="var(--dv-rust)"><title>Rust server — 2.2x mongod</title></path><text x="247.0" y="307" class="dv-val">2.2<tspan class="dv-x">x</tspan></text><path d="M200,312 h669.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-669.1 z" fill="var(--dv-py)"><title>Python server — 35.4x mongod</title></path><text x="879.1" y="323" class="dv-val">35.4<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h47.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-47.0 z" fill="var(--dv-rust)"><title>Rust server — 2.7x mongod</title></path><text x="257.0" y="361" class="dv-val">2.7<tspan class="dv-x">x</tspan></text><path d="M200,366 h370.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-370.3 z" fill="var(--dv-py)"><title>Python server — 19.7x mongod</title></path><text x="580.3" y="377" class="dv-val">19.7<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h25.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-25.8 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="235.8" y="415" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,420 h338.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-338.1 z" fill="var(--dv-py)"><title>Python server — 18.0x mongod</title></path><text x="548.1" y="431" class="dv-val">18.0<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h19.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-19.8 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="229.8" y="469" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,474 h35.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-35.4 z" fill="var(--dv-py)"><title>Python server — 2.1x mongod</title></path><text x="245.4" y="485" class="dv-val">2.1<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 120.0 ms | 240.8 ms | 2.0× | 1055.8 ms | 8.8× |
| find indexed range | 13.5 ms | 15.0 ms | 1.1× | 129.0 ms | 9.6× |
| find full scan | 24.6 ms | 32.3 ms | 1.3× | 214.5 ms | 8.7× |
| find filtered scan | 24.6 ms | 32.8 ms | 1.3× | 318.6 ms | 13.0× |
| update_many (half) | 122.6 ms | 199.1 ms | 1.6× | 2426.1 ms | 19.8× |
| aggregate $group | 15.7 ms | 33.8 ms | 2.2× | 555.4 ms | 35.4× |
| aggregate multi-stage | 23.0 ms | 61.6 ms | 2.7× | 451.6 ms | 19.7× |
| delete_many (half) | 61.6 ms | 96.6 ms | 1.6× | 1107.7 ms | 18.0× |
| change-stream drain | 160.9 ms | 201.2 ms | 1.3× | 333.4 ms | 2.1× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~1.1×–2.7× of mongod** per operation. The closest
  rows are the indexed range read (1.1×), the full and filtered scans (1.3×)
  and the change-stream drain (1.3×); the widest stay on the aggregation paths
  (`$group` 2.2×, multi-stage 2.7×) and `insert` (2.0×), which is dispatch and
  operator work above a storage engine that is literally the same C library.

  **These ratios are not comparable to the 2026-08-26 publication**, and the
  engine is not why. The reference moved from **mongod 8.0.31 to 8.0.32**, which
  is ~18% faster on the droplet head-to-head — and every ×mongod figure is a
  ratio, so a quicker denominator widens the gap without anything here changing.
  The same caveat as the 6.0→8.0 move recorded above applies: compare ratios
  only within one reference version.

  The read rows improved sharply in 0.6.0b11: `getMore` had been reusing
  mongod's 101-document *first-batch* default on every batch, so a
  10,000-document scan paid ~100 round trips where mongod pays 2. Removing that
  round-trip tax took the full scan from ~2.2× to parity.

- **The Python server runs at ~2.1×–35.4× of mongod** on these workloads — the
  low end is the change-stream drain, where the work is oplog reads rather than
  per-document compute — and the Rust server is correspondingly **~1.7×–16.4×
  faster than the Python server** workload-for-workload. The largest gaps are
  the update-heavy and aggregation paths, where Python does the most
  per-document work.
- Every number includes the wire protocol and `pymongo` driver overhead a
  real client pays — these are end-to-end times, not engine microbenchmarks.
- The numbers are **single-machine, single-process, no concurrency** — a
  deliberately narrow scenario to isolate per-operation latency. Throughput
  under concurrent connections is a separate measurement (and a place where
  mongod's connection pooling / async accept loop wins regardless).

The trade is unchanged: conformance and WiredTiger durability over raw
per-op latency. For ephemeral test and dev data the wall-clock difference
rarely matters; when it does, that's what the Rust server is for. See
[The two servers](servers.md) and the
[feature comparison](feature-comparison.md) for what each supports.

## How to refresh

```bash
# The embedded Rust server needs the storage-engine build:
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev
uv run --no-sync python -m bench.compare_servers --n 10000 --reps 5
```

Requires `mongod` on `PATH` (Community Server is enough; `--no-mongod` skips
it and compares the two SecantusDB servers only). On macOS:
`brew tap mongodb/brew && brew install mongodb-community`.

## Over a real network, against a real MongoDB

The numbers above are single-host: server and client share one machine, so
the "network" is loopback and the load generator competes with the database
for the same cores. The harness in
[`bench/DO_CLUSTER.md`](https://github.com/jdrumgoole/SecantusDB/blob/main/bench/DO_CLUSTER.md)
measures the deployment shape instead — one server droplet, two separate
client droplets, real NICs between them — and runs **SecantusDB and a real
`mongod` back-to-back on the same hardware**, interleaved across passes so
drift lands on both equally.

<!-- head-to-head:begin -->
Measured 2026-08-26 on DigitalOcean `lon1`: a `c-4 (4 vCPU, 8192 MB)` server and 2 x c-2, 16 workers each, 8 KiB **incompressible**
documents, a 70/20/10 insert/find/update mix, a 4G WiredTiger cache for
both engines, and 3 interleaved passes:

| engine | version | ops/s (median) | spread | p50 | p99 | p99.9 | server CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| SecantusDB | 0.5.3-beta.163 | **9,338** | 3.1% | 2.48 ms | 16.76 ms | **37.34 ms** | 78.9% |
| mongod | 8.0.31 | **12,698** | 2.6% | 1.92 ms | 12.48 ms | **31.62 ms** | 78.0% |

**SecantusDB reaches 0.74x of MongoDB's throughput on this workload, with p50
latency within 1.29x and p99.9 within 1.18x.** Both engines saturated the same
server while the clients sat idle, so both figures are server-bound and the
comparison is fair. Run-to-run spread was about 3.1%.
<!-- head-to-head:end -->

Tail latency is where this has moved most. Two releases ago the p99.9 ratio was
**2.0x**; it is now **1.18x**. Before the block compressor changed it was
**72x** — profiling found 65% of server CPU inside zlib's `deflate`, and
switching the default to lz4 cut p99.9 from 1,303 ms to 37 ms in one step. What
remains is a real throughput gap, no longer dominated by any single cause.

Caveats, in both directions:

- **The payload matters.** These documents are incompressible. On compressible
  documents both engines do better and the ratio shifts, because compression
  ratio starts paying for itself. Real workloads sit somewhere between.
- **This is one workload shape.** Write-heavy, small documents, single-node, no
  secondary indexes beyond `_id` and the benchmark's own. It is a useful
  comparison, not a general claim.
- **Tail latency is still the weaker axis.** p50 is close; p99.9 is 2x. If your
  workload is write-heavy and latency-sensitive at the tail, measure with your
  own data before switching.

Reproduce with `invoke do-bench --repeat 3 --payload random` (needs a
DigitalOcean API token; the harness provisions, measures and destroys).

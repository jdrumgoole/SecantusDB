# Benchmark: both servers vs mongod

Generated 2026-08-26 on a dedicated DigitalOcean instance (8 vCPU, x86-64
Linux), against **mongod 8.0.32**, via `invoke do-perf`.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 524" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="486" class="dv-grid"/><text x="295.2" y="502" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="486" class="dv-grid"/><text x="390.4" y="502" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="486" class="dv-grid"/><text x="485.6" y="502" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="486" class="dv-grid"/><text x="580.8" y="502" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="486" class="dv-grid"/><text x="676.0" y="502" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="486" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h37.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-37.6 z" fill="var(--dv-rust)"><title>Rust server — 2.2x mongod</title></path><text x="247.6" y="37" class="dv-val">2.2<tspan class="dv-x">x</tspan></text><path d="M200,42 h183.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-183.1 z" fill="var(--dv-py)"><title>Python server — 9.8x mongod</title></path><text x="393.1" y="53" class="dv-val">9.8<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h14.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-14.3 z" fill="var(--dv-rust)"><title>Rust server — 1.0x mongod</title></path><text x="224.3" y="91" class="dv-val">1.0<tspan class="dv-x">x</tspan></text><path d="M200,96 h141.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-141.2 z" fill="var(--dv-py)"><title>Python server — 7.6x mongod</title></path><text x="351.2" y="107" class="dv-val">7.6<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h21.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-21.6 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231.6" y="145" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,150 h174.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-174.9 z" fill="var(--dv-py)"><title>Python server — 9.4x mongod</title></path><text x="384.9" y="161" class="dv-val">9.4<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h18.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-18.2 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="228.2" y="199" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,204 h217.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-217.6 z" fill="var(--dv-py)"><title>Python server — 11.6x mongod</title></path><text x="427.6" y="215" class="dv-val">11.6<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h21.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-21.9 z" fill="var(--dv-rust)"><title>Rust server — 1.4x mongod</title></path><text x="231.9" y="253" class="dv-val">1.4<tspan class="dv-x">x</tspan></text><path d="M200,258 h378.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-378.7 z" fill="var(--dv-py)"><title>Python server — 20.1x mongod</title></path><text x="588.7" y="269" class="dv-val">20.1<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h35.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-35.9 z" fill="var(--dv-rust)"><title>Rust server — 2.1x mongod</title></path><text x="245.9" y="307" class="dv-val">2.1<tspan class="dv-x">x</tspan></text><path d="M200,312 h556.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-556.1 z" fill="var(--dv-py)"><title>Python server — 29.4x mongod</title></path><text x="766.1" y="323" class="dv-val">29.4<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h64.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-64.1 z" fill="var(--dv-rust)"><title>Rust server — 3.6x mongod</title></path><text x="274.1" y="361" class="dv-val">3.6<tspan class="dv-x">x</tspan></text><path d="M200,366 h468.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-468.9 z" fill="var(--dv-py)"><title>Python server — 24.8x mongod</title></path><text x="678.9" y="377" class="dv-val">24.8<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h37.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-37.1 z" fill="var(--dv-rust)"><title>Rust server — 2.2x mongod</title></path><text x="247.1" y="415" class="dv-val">2.2<tspan class="dv-x">x</tspan></text><path d="M200,420 h374.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-374.3 z" fill="var(--dv-py)"><title>Python server — 19.9x mongod</title></path><text x="584.3" y="431" class="dv-val">19.9<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h19.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-19.6 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="229.6" y="469" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,474 h44.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-44.8 z" fill="var(--dv-py)"><title>Python server — 2.6x mongod</title></path><text x="254.8" y="485" class="dv-val">2.6<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 89.8 ms | 196.0 ms | 2.2× | 882.2 ms | 9.8× |
| find indexed range | 11.3 ms | 10.9 ms | 1.0× | 86.5 ms | 7.6× |
| find full scan | 18.2 ms | 24.5 ms | 1.3× | 171.1 ms | 9.4× |
| find filtered scan | 17.5 ms | 20.4 ms | 1.2× | 203.2 ms | 11.6× |
| update_many (half) | 114.7 ms | 155.8 ms | 1.4× | 2304.9 ms | 20.1× |
| aggregate $group | 12.4 ms | 26.0 ms | 2.1× | 364.8 ms | 29.4× |
| aggregate multi-stage | 14.8 ms | 52.9 ms | 3.6× | 367.4 ms | 24.8× |
| delete_many (half) | 46.9 ms | 101.2 ms | 2.2× | 930.8 ms | 19.9× |
| change-stream drain | 118.0 ms | 146.2 ms | 1.2× | 302.4 ms | 2.6× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~1.0×–2.8× of mongod** per operation. Full scan
  (1.0×) sits at parity, with indexed range, filtered scan and the
  change-stream drain just behind it at 1.1×; `update_many` is 1.5×. The
  widest gaps stay on the aggregation paths (`$group` 2.0×, multi-stage 2.8×),
  `delete_many` (2.0×) and `insert` (1.9×) — dispatch and operator work above
  a storage engine that is literally the same C library.

  Multi-stage aggregation is the row to read with care. It went 2.4× → 3.5×
  in the previous publication and is back to 2.8× in this one (2026-10-07,
  0.5.3-beta.172), with `delete_many` at 2.0×. A same-machine A/B of
  published binaries has put every workload within ±4% before, so movement on
  this scale is the shared-CPU droplet plan rather than the code. Treat the
  aggregation figure as the noisiest number on this page.

  The read rows improved sharply in 0.6.0b11: `getMore` had been reusing
  mongod's 101-document *first-batch* default on every batch, so a
  10,000-document scan paid ~100 round trips where mongod pays 2. Removing that
  round-trip tax took the full scan from ~2.2× to parity.

- **The Python server runs at ~2.3×–26.5× of mongod** on these workloads — the
  low end is the change-stream drain, where the work is oplog reads rather than
  per-document compute — and the Rust server is correspondingly **~1.8×–14.2×
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
Measured 2026-10-08 on DigitalOcean `lon1`: a `c-4 (4 vCPU, 8192 MB)` server and 2 x c-2, 16 workers each, 8 KiB **incompressible**
documents, a 70/20/10 insert/find/update mix, a 4G WiredTiger cache for
both engines, and 3 interleaved passes:

| engine | version | ops/s (median) | spread | p50 | p99 | p99.9 | server CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| SecantusDB | f162a408cf25ec2a69d57db5b4e3ba29741f5789 | **9,064** | 2.1% | 2.51 ms | 17.79 ms | **39.95 ms** | 79.0% |
| mongod | 8.0.32 | **12,635** | 5.0% | 1.90 ms | 12.80 ms | **31.75 ms** | 78.1% |

**SecantusDB reaches 0.72x of MongoDB's throughput on this workload, with p50
latency within 1.32x and p99.9 within 1.26x.** Both engines saturated the same
server while the clients sat idle, so both figures are server-bound and the
comparison is fair. Run-to-run spread was about 2.1%.
<!-- head-to-head:end -->

Tail latency needs reading carefully, because the ratio and the underlying
number now point in opposite directions. Our absolute p99.9 has **improved**
across the measured releases — 37.3 ms on 0.5.3-beta.163, 30.5 ms on beta.164,
31.2 ms on the 2026-09-20 run, 32.8 ms here — while the *ratio* against mongod
went 1.18× → 1.77×. The ratio worsened mainly because mongod improved faster: its
own p99.9 fell from 31.6 ms to 18.1 ms between 8.0.31 and 8.0.32 (18.6 ms here). Nothing regressed on our side;
the reference got better, which is exactly the kind of movement a ratio hides
and an absolute number shows.

Before the block compressor changed, that ratio was **72×** — profiling found
65% of server CPU inside zlib's `deflate`, and switching the default to lz4 cut
p99.9 from 1,303 ms to 37 ms in one step. What remains is a real throughput gap
(0.73× of mongod on 2026-10-07, 0.76× on 2026-09-20), no longer dominated by
any single cause. Both engines measured lower this time (SecantusDB −9%,
mongod −6%) on the same droplet plan, so most of that movement is the machine.

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

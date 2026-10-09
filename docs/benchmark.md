# Benchmark: both servers vs mongod

Generated 2026-10-09 on a dedicated DigitalOcean instance (8 vCPU, x86-64
Linux), against **mongod 8.0.32**, via `invoke do-perf`: fifteen reps, with
the three servers interleaved inside each rep. The chart and table show the
newest run; the prose under them quotes the range across three such runs
(see "Three interleaved runs").

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 524" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="486" class="dv-grid"/><text x="295.2" y="502" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="486" class="dv-grid"/><text x="390.4" y="502" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="486" class="dv-grid"/><text x="485.6" y="502" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="486" class="dv-grid"/><text x="580.8" y="502" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="486" class="dv-grid"/><text x="676.0" y="502" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="486" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h35.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-35.1 z" fill="var(--dv-rust)"><title>Rust server — 2.1x mongod</title></path><text x="245.1" y="37" class="dv-val">2.1<tspan class="dv-x">x</tspan></text><path d="M200,42 h169.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-169.5 z" fill="var(--dv-py)"><title>Python server — 9.1x mongod</title></path><text x="379.5" y="53" class="dv-val">9.1<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h14.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-14.7 z" fill="var(--dv-rust)"><title>Rust server — 1.0x mongod</title></path><text x="224.7" y="91" class="dv-val">1.0<tspan class="dv-x">x</tspan></text><path d="M200,96 h158.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-158.0 z" fill="var(--dv-py)"><title>Python server — 8.5x mongod</title></path><text x="368.0" y="107" class="dv-val">8.5<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h16.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-16.6 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="226.6" y="145" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,150 h132.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-132.1 z" fill="var(--dv-py)"><title>Python server — 7.2x mongod</title></path><text x="342.1" y="161" class="dv-val">7.2<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h21.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-21.6 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231.6" y="199" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,204 h245.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-245.3 z" fill="var(--dv-py)"><title>Python server — 13.1x mongod</title></path><text x="455.3" y="215" class="dv-val">13.1<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h23.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-23.0 z" fill="var(--dv-rust)"><title>Rust server — 1.4x mongod</title></path><text x="233.0" y="253" class="dv-val">1.4<tspan class="dv-x">x</tspan></text><path d="M200,258 h359.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-359.4 z" fill="var(--dv-py)"><title>Python server — 19.1x mongod</title></path><text x="569.4" y="269" class="dv-val">19.1<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h32.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-32.1 z" fill="var(--dv-rust)"><title>Rust server — 1.9x mongod</title></path><text x="242.1" y="307" class="dv-val">1.9<tspan class="dv-x">x</tspan></text><path d="M200,312 h448.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-448.6 z" fill="var(--dv-py)"><title>Python server — 23.8x mongod</title></path><text x="658.6" y="323" class="dv-val">23.8<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h44.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-44.7 z" fill="var(--dv-rust)"><title>Rust server — 2.6x mongod</title></path><text x="254.7" y="361" class="dv-val">2.6<tspan class="dv-x">x</tspan></text><path d="M200,366 h405.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-405.1 z" fill="var(--dv-py)"><title>Python server — 21.5x mongod</title></path><text x="615.1" y="377" class="dv-val">21.5<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h31.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-31.8 z" fill="var(--dv-rust)"><title>Rust server — 1.9x mongod</title></path><text x="241.8" y="415" class="dv-val">1.9<tspan class="dv-x">x</tspan></text><path d="M200,420 h325.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-325.2 z" fill="var(--dv-py)"><title>Python server — 17.3x mongod</title></path><text x="535.2" y="431" class="dv-val">17.3<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h19.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-19.4 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="229.4" y="469" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,474 h40.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-40.4 z" fill="var(--dv-py)"><title>Python server — 2.3x mongod</title></path><text x="250.4" y="485" class="dv-val">2.3<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 93.0 ms | 190.7 ms | 2.1× | 847.5 ms | 9.1× |
| find indexed range | 11.3 ms | 11.2 ms | 1.0× | 96.6 ms | 8.5× |
| find full scan | 20.9 ms | 22.6 ms | 1.1× | 149.2 ms | 7.2× |
| find filtered scan | 16.0 ms | 21.5 ms | 1.3× | 209.5 ms | 13.1× |
| update_many (half) | 109.8 ms | 155.4 ms | 1.4× | 2095.0 ms | 19.1× |
| aggregate $group | 14.3 ms | 27.1 ms | 1.9× | 339.4 ms | 23.8× |
| aggregate multi-stage | 16.5 ms | 42.3 ms | 2.6× | 355.4 ms | 21.5× |
| delete_many (half) | 56.0 ms | 105.2 ms | 1.9× | 967.7 ms | 17.3× |
| change-stream drain | 115.0 ms | 141.4 ms | 1.2× | 268.1 ms | 2.3× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~1.0×–3.2× of mongod** per operation, taking
  the lowest and highest figure across three interleaved runs. The reads
  sit closest: indexed range at 1.0×–1.2×, full scan at
  1.1×–1.4×, filtered scan at 1.2×–1.3× and the change-stream drain at
  1.1×–1.3×; `update_many` is 1.4×–1.7×. The widest gaps stay on
  `insert` (2.1×–2.3×), `delete_many` (1.9×–2.1×) and the aggregation
  paths (`$group` 1.9×–2.3×, multi-stage 2.6×–3.2×) — dispatch and
  operator work above a storage engine that is literally the same C
  library.

  The previous publication gave this range as 1.0×–4.1×. Its top end was
  the multi-stage aggregation in one run of a harness that measured the
  servers one after another; measured interleaved, the same row is
  3.2×, 2.9× and 2.6×. It is still the row that moves most between
  runs. See "Three interleaved runs" below.

  The read rows improved sharply in 0.6.0b11: `getMore` had been reusing
  mongod's 101-document *first-batch* default on every batch, so a
  10,000-document scan paid ~100 round trips where mongod pays 2. Removing that
  round-trip tax took the full scan from ~2.2× to parity.

- **The Python server runs at ~2.0×–26.7× of mongod** on these workloads — the
  low end is the change-stream drain, where the work is oplog reads rather than
  per-document compute — and the Rust server is correspondingly **~1.9×–13.5×
  faster than the Python server** workload-for-workload. The largest gaps are
  the update-heavy and aggregation paths, where Python does the most
  per-document work.
- Every number includes the wire protocol and `pymongo` driver overhead a
  real client pays — these are end-to-end times, not engine microbenchmarks.
- The numbers are **single-machine, single-process, no concurrency** — a
  deliberately narrow scenario to isolate per-operation latency. Throughput
  under concurrent connections is a separate measurement (and a place where
  mongod's connection pooling / async accept loop wins regardless).

### Three sequential runs, one build

The per-operation harness was run three times on the same engine code —
once on 2026-10-07 and twice on 2026-10-08, each on a freshly provisioned
droplet. Nothing in the MongoDB-side engine changed between them, so the
differences are the measurement, not the server. Rust server, ×mongod:

| Workload | 2026-10-07 | 2026-10-08 (a) | 2026-10-08 (b) |
|---|---:|---:|---:|
| insert (10k docs) | 1.95× | 2.18× | 2.08× |
| find indexed range | 1.08× | 0.96× | 1.37× |
| find full scan | 1.02× | 1.34× | 1.32× |
| find filtered scan | 1.26× | 1.17× | 1.23× |
| update_many (half) | 1.48× | 1.36× | 1.45× |
| aggregate $group | 1.99× | 2.10× | 2.72× |
| aggregate multi-stage | 2.83× | 3.58× | 4.07× |
| delete_many (half) | 1.97× | 2.16× | 3.00× |
| change-stream drain | 1.24× | 1.24× | 1.22× |

Four rows reproduce to within about ±0.1× (insert, filtered scan,
`update_many`, change-stream drain). The two aggregation rows and
`delete_many` do not: they moved by 0.7×–1.2× with no code change.
`mongod`'s own timings moved too — its multi-stage aggregation took
23.7 ms on the first droplet and 14.5 ms on the third — so part of the
swing is the denominator. A single run of this harness cannot tell a
30% change on those three rows from noise; use the range.

### Three interleaved runs

The three runs above measured all of `mongod`'s reps, then all of the Rust
server's, then all of the Python server's, five each, so a change in the
machine during a run landed on one column of a ratio. From 2026-10-09 the
harness runs every server within each rep, rotating the order, and the
droplet task takes fifteen reps. Three droplet runs that way, all on
2026-10-09, on machines whose absolute speed differed by about 1.5×. The
third measured 0.5.3-beta.174, whose only engine change from the build in the
first two is the `$natural` sort fix. Rust server, ×mongod:

| Workload | run 1 | run 2 | run 3 |
|---|---:|---:|---:|
| insert (10k docs) | 2.07× | 2.28× | 2.05× |
| find indexed range | 1.14× | 1.17× | 0.98× |
| find full scan | 1.16× | 1.42× | 1.08× |
| find filtered scan | 1.16× | 1.17× | 1.35× |
| update_many (half) | 1.71× | 1.36× | 1.42× |
| aggregate $group | 2.04× | 2.34× | 1.90× |
| aggregate multi-stage | 3.19× | 2.95× | 2.56× |
| delete_many (half) | 2.05× | 2.07× | 1.88× |
| change-stream drain | 1.25× | 1.08× | 1.23× |

Against the sequential runs:

| Workload | sequential, 5 reps (3 runs) | interleaved, 15 reps (3 runs) |
|---|---:|---:|
| insert (10k docs) | 1.95×–2.18× | 2.05×–2.28× |
| find indexed range | 0.96×–1.37× | 0.98×–1.17× |
| find full scan | 1.02×–1.34× | 1.08×–1.42× |
| find filtered scan | 1.17×–1.26× | 1.16×–1.35× |
| update_many (half) | 1.36×–1.48× | 1.36×–1.71× |
| aggregate $group | 1.99×–2.72× | 1.90×–2.34× |
| aggregate multi-stage | 2.83×–4.07× | 2.56×–3.19× |
| delete_many (half) | 1.97×–3.00× | 1.88×–2.07× |
| change-stream drain | 1.22×–1.24× | 1.08×–1.25× |

The widest disagreement between runs fell from 1.24× to 0.63×, both on the
multi-stage aggregation. `delete_many` went from 1.03× to 0.19× and `$group`
from 0.73× to 0.44×. Three rows moved the other way: `update_many` now spans
0.35×, filtered scan 0.19× and the change-stream drain 0.17×, where the
sequential runs had them within 0.12×. So interleaving removed the large
swings and did not make every row hold still; a 20% change in one run of
this harness is still inside the noise on most rows. The change-stream row's
`mongod` figure comes from a separate replica-set run and is not interleaved.

The trade is unchanged: conformance and WiredTiger durability over raw
per-op latency. For ephemeral test and dev data the wall-clock difference
rarely matters; when it does, that's what the Rust server is for. See
[The two servers](servers.md) and the
[feature comparison](feature-comparison.md) for what each supports.

## How to refresh

```bash
# The embedded Rust server needs the storage-engine build:
SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev
uv run --no-sync python -m bench.compare_servers --n 10000 --reps 15
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
| SecantusDB | f162a408cf25ec2a69d57db5b4e3ba29741f5789 | **11,703** | 8.6% | 2.02 ms | 14.02 ms | **29.19 ms** | 78.7% |
| mongod | 8.0.32 | **15,365** | 3.3% | 1.64 ms | 9.92 ms | **17.41 ms** | 80.1% |

**SecantusDB reaches 0.76x of MongoDB's throughput on this workload, with p50
latency within 1.23x and p99.9 within 1.68x.** Both engines saturated the same
server while the clients sat idle, so both figures are server-bound and the
comparison is fair. Run-to-run spread was about 8.6%.
<!-- head-to-head:end -->

This workload was measured three times on the same build — once on
2026-10-07 and twice on 2026-10-08, on freshly provisioned droplets each
time. The table above is the newest; all three:

| run | SecantusDB ops/s | mongod ops/s | throughput | p50 | p99 | p99.9 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 2026-10-07 | 10,175 | 13,928 | 0.73× | 1.25× | 1.49× | 1.77× |
| 2026-10-08 (a) | 9,064 | 12,635 | 0.72× | 1.32× | 1.39× | 1.26× |
| 2026-10-08 (b) | 11,703 | 15,365 | 0.76× | 1.23× | 1.41× | 1.68× |

The throughput ratio is the stable figure: 0.72×–0.76× while the absolute
rate of both engines moved by 20%–30% between droplets. p50 and p99 hold
to 1.2×–1.3× and 1.4×–1.5×. In the newest run SecantusDB's three passes
spread 8.6%, wider than the 5% this page normally asks of a published
figure, so read its 11,703 as 10,900–11,900.

**The p99.9 ratio is not stable, and an earlier version of this page read
too much into it.** It credited a rise from 1.18× to 1.77× to `mongod`
getting faster between 8.0.31 and 8.0.32. The three runs above are all
8.0.32, and `mongod`'s p99.9 was 18.6 ms, 31.8 ms and 17.4 ms across them;
ours was 32.8 ms, 40.0 ms and 29.2 ms. That is a droplet-to-droplet swing
as large as the one attributed to the version change, so this benchmark
does not support that explanation. What it does support: our p99.9 is
1.3×–1.8× of `mongod`'s, and has stayed between 29 ms and 40 ms since
0.5.3-beta.163 (37.3 ms).

Before the block compressor changed, that ratio was **72×** — profiling found
65% of server CPU inside zlib's `deflate`, and switching the default to lz4 cut
p99.9 from 1,303 ms to 37 ms in one step. What remains is a real throughput gap
(0.72×–0.76× of mongod across the three runs above, 0.76× on 2026-09-20), no
longer dominated by any single cause.

Caveats, in both directions:

- **The payload matters.** These documents are incompressible. On compressible
  documents both engines do better and the ratio shifts, because compression
  ratio starts paying for itself. Real workloads sit somewhere between.
- **This is one workload shape.** Write-heavy, small documents, single-node, no
  secondary indexes beyond `_id` and the benchmark's own. It is a useful
  comparison, not a general claim.
- **Tail latency is still the weaker axis.** p50 is close; p99.9 is 1.3×–1.8×. If your
  workload is write-heavy and latency-sensitive at the tail, measure with your
  own data before switching.

Reproduce with `invoke do-bench --repeat 3 --payload random` (needs a
DigitalOcean API token; the harness provisions, measures and destroys).

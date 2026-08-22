# Benchmark: both servers vs mongod

Generated 2026-08-22 on a dedicated DigitalOcean droplet (4 vCPU, x86-64
Linux), against **mongod 8.0.29**, via `invoke do-perf`.

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

- **The reference moved from mongod 6.0.16 to 8.0.29.** Every `×mongod` figure
  is a ratio, so a faster denominator makes us look worse. mongod 8.0 is
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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 524" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="486" class="dv-grid"/><text x="295.2" y="502" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="486" class="dv-grid"/><text x="390.4" y="502" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="486" class="dv-grid"/><text x="485.6" y="502" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="486" class="dv-grid"/><text x="580.8" y="502" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="486" class="dv-grid"/><text x="676.0" y="502" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="486" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h34.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-34.4 z" fill="var(--dv-rust)"><title>Rust server — 2.0x mongod</title></path><text x="244.4" y="37" class="dv-val">2.0<tspan class="dv-x">x</tspan></text><path d="M200,42 h188.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-188.5 z" fill="var(--dv-py)"><title>Python server — 10.1x mongod</title></path><text x="398.5" y="53" class="dv-val">10.1<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h19.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-19.2 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="229.2" y="91" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,96 h152.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-152.2 z" fill="var(--dv-py)"><title>Python server — 8.2x mongod</title></path><text x="362.2" y="107" class="dv-val">8.2<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h18.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-18.4 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="228.4" y="145" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,150 h177.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-177.5 z" fill="var(--dv-py)"><title>Python server — 9.5x mongod</title></path><text x="387.5" y="161" class="dv-val">9.5<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h22.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-22.5 z" fill="var(--dv-rust)"><title>Rust server — 1.4x mongod</title></path><text x="232.5" y="199" class="dv-val">1.4<tspan class="dv-x">x</tspan></text><path d="M200,204 h261.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-261.0 z" fill="var(--dv-py)"><title>Python server — 13.9x mongod</title></path><text x="471.0" y="215" class="dv-val">13.9<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h20.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-20.6 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="230.6" y="253" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,258 h313.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-313.2 z" fill="var(--dv-py)"><title>Python server — 16.7x mongod</title></path><text x="523.2" y="269" class="dv-val">16.7<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h30.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-30.7 z" fill="var(--dv-rust)"><title>Rust server — 1.8x mongod</title></path><text x="240.7" y="307" class="dv-val">1.8<tspan class="dv-x">x</tspan></text><path d="M200,312 h443.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-443.0 z" fill="var(--dv-py)"><title>Python server — 23.5x mongod</title></path><text x="653.0" y="323" class="dv-val">23.5<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h54.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-54.4 z" fill="var(--dv-rust)"><title>Rust server — 3.1x mongod</title></path><text x="264.4" y="361" class="dv-val">3.1<tspan class="dv-x">x</tspan></text><path d="M200,366 h376.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-376.9 z" fill="var(--dv-py)"><title>Python server — 20.0x mongod</title></path><text x="586.9" y="377" class="dv-val">20.0<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h30.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-30.7 z" fill="var(--dv-rust)"><title>Rust server — 1.8x mongod</title></path><text x="240.7" y="415" class="dv-val">1.8<tspan class="dv-x">x</tspan></text><path d="M200,420 h345.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-345.2 z" fill="var(--dv-py)"><title>Python server — 18.3x mongod</title></path><text x="555.2" y="431" class="dv-val">18.3<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h15.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-15.9 z" fill="var(--dv-rust)"><title>Rust server — 1.0x mongod</title></path><text x="225.9" y="469" class="dv-val">1.0<tspan class="dv-x">x</tspan></text><path d="M200,474 h34.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-34.0 z" fill="var(--dv-py)"><title>Python server — 2.0x mongod</title></path><text x="244.0" y="485" class="dv-val">2.0<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 46.6 ms | 94.0 ms | 2.0× | 471.1 ms | 10.1× |
| find indexed range | 5.9 ms | 7.2 ms | 1.2× | 48.1 ms | 8.2× |
| find full scan | 9.8 ms | 11.5 ms | 1.2× | 93.0 ms | 9.5× |
| find filtered scan | 7.9 ms | 11.0 ms | 1.4× | 110.2 ms | 13.9× |
| update_many (half) | 60.2 ms | 77.8 ms | 1.3× | 1002.8 ms | 16.7× |
| aggregate $group | 7.2 ms | 13.1 ms | 1.8× | 168.8 ms | 23.5× |
| aggregate multi-stage | 9.3 ms | 28.6 ms | 3.1× | 186.3 ms | 20.0× |
| delete_many (half) | 28.8 ms | 52.4 ms | 1.8× | 527.2 ms | 18.3× |
| change-stream drain | 62.6 ms | 65.5 ms | 1.0× | 125.0 ms | 2.0× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~1.0×–3.1× of mongod** per operation. The closest
  row is the change-stream drain at 1.0× — parity with mongod at its own change
  streams. Indexed range and full scan sit at 1.2×, `update_many` at 1.3×. The
  widest gaps are the aggregation paths (`$group` 1.8×, multi-stage 3.1×) and
  `insert` at 2.0×, which is dispatch and operator work above a storage engine
  that is literally the same C library.

  **These ratios are worse than the figures published before 2026-08-22, and
  the engine is not the reason.** SecantusDB's absolute timings barely moved;
  the reference did. Against mongod 6.0.16 on a laptop the same build measured
  1.4× on insert, against 8.0.29 on a droplet it measures 2.0× — because
  mongod 8.0 inserts substantially faster than 6.0, not because SecantusDB
  slowed down. Publishing against a three-major-version-old mongod flattered
  us; this is the honest comparison against what a user would actually deploy.

- **The Python server runs at ~2.0×–23.5× of mongod** on these workloads — the
  2.0× is the change-stream drain, where the work is oplog reads rather than
  per-document compute — and the Rust server is correspondingly **~1.9×–12.9×
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

Measured 2026-08-22 on DigitalOcean `lon1`: a `c-4` server (4 dedicated vCPU,
8 GB) and two `c-2` clients, 16 workers each, 8 KiB **incompressible**
documents, a 70/20/10 insert/find/update mix, 4 GB WiredTiger cache for both
engines, three interleaved 90-second passes:

| engine | version | ops/s (median) | spread | p50 | p99 | p99.9 | server CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| SecantusDB | 0.5.3-beta.161 | **11,099** | 3.1% | 2.06 ms | 15 ms | **37 ms** | 78.3% |
| mongod | 8.0.29 | **14,772** | 1.5% | 1.71 ms | 10 ms | **18 ms** | 80.2% |

**SecantusDB reaches about three quarters of MongoDB's throughput on this
workload (0.75x), with p50 latency within 1.2x and p99.9 within 2.0x.** Both
engines saturated the same server (78-80% CPU) while the clients sat idle, so
both figures are server-bound and the comparison is fair. Run-to-run spread was
about 3%.

That is a large improvement on the previous release, which measured 0.27x
throughput and **72x** the p99.9 latency. The difference is the block
compressor: profiling found 65% of server CPU inside zlib's `deflate`, and
switching the default to lz4 took SecantusDB from 3,993 to 11,099 ops/s and cut
p99.9 from 1,303 ms to 37 ms on this benchmark. The remaining gap is real but
no longer dominated by one cause.

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

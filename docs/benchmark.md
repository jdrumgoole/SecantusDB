# Benchmark: both servers vs mongod

Generated 2026-08-11 on Darwin arm64 (Apple Silicon), `bench.compare_servers`.

All three servers use the **same WiredTiger storage engine** — mongod ships
it; SecantusDB vendors the same C library — driven by the same `pymongo`
client over the wire protocol. The hot path differs only above the storage
layer (command dispatch, query planner, operator engines), so this is a fair
comparison of the parts of SecantusDB that aren't WiredTiger itself.

Each workload runs against a freshly-spawned server on a free port with its
own tmp data dir, all on-disk WiredTiger. Each timed 5× per server; the table
reports the median in milliseconds and how many times slower than `mongod`
each server is. Dataset is 10,000 small docs.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 524" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="486" class="dv-grid"/><text x="295.2" y="502" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="486" class="dv-grid"/><text x="390.4" y="502" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="486" class="dv-grid"/><text x="485.6" y="502" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="486" class="dv-grid"/><text x="580.8" y="502" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="486" class="dv-grid"/><text x="676.0" y="502" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="486" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h17.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-17.9 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="227.9" y="37" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,42 h68.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-68.8 z" fill="var(--dv-py)"><title>Python server — 3.8x mongod</title></path><text x="278.8" y="53" class="dv-val">3.8<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h22.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-22.2 z" fill="var(--dv-rust)"><title>Rust server — 1.4x mongod</title></path><text x="232.2" y="91" class="dv-val">1.4<tspan class="dv-x">x</tspan></text><path d="M200,96 h121.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-121.9 z" fill="var(--dv-py)"><title>Python server — 6.6x mongod</title></path><text x="331.9" y="107" class="dv-val">6.6<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h38.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-38.4 z" fill="var(--dv-rust)"><title>Rust server — 2.2x mongod</title></path><text x="248.4" y="145" class="dv-val">2.2<tspan class="dv-x">x</tspan></text><path d="M200,150 h139.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-139.8 z" fill="var(--dv-py)"><title>Python server — 7.6x mongod</title></path><text x="349.8" y="161" class="dv-val">7.6<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h26.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.3 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236.3" y="199" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,204 h222.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-222.9 z" fill="var(--dv-py)"><title>Python server — 11.9x mongod</title></path><text x="432.9" y="215" class="dv-val">11.9<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h16.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-16.3 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="226.3" y="253" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,258 h239.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-239.9 z" fill="var(--dv-py)"><title>Python server — 12.8x mongod</title></path><text x="449.9" y="269" class="dv-val">12.8<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h13.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-13.9 z" fill="var(--dv-rust)"><title>Rust server — 0.9x mongod</title></path><text x="223.9" y="307" class="dv-val">0.9<tspan class="dv-x">x</tspan></text><path d="M200,312 h321.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-321.7 z" fill="var(--dv-py)"><title>Python server — 17.1x mongod</title></path><text x="531.7" y="323" class="dv-val">17.1<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h26.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.7 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236.7" y="361" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,366 h448.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-448.1 z" fill="var(--dv-py)"><title>Python server — 23.7x mongod</title></path><text x="658.1" y="377" class="dv-val">23.7<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h13.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-13.9 z" fill="var(--dv-rust)"><title>Rust server — 0.9x mongod</title></path><text x="223.9" y="415" class="dv-val">0.9<tspan class="dv-x">x</tspan></text><path d="M200,420 h225.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-225.6 z" fill="var(--dv-py)"><title>Python server — 12.1x mongod</title></path><text x="435.6" y="431" class="dv-val">12.1<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h8.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-8.4 z" fill="var(--dv-rust)"><title>Rust server — 0.7x mongod</title></path><text x="218.4" y="469" class="dv-val">0.7<tspan class="dv-x">x</tspan></text><path d="M200,474 h19.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-19.1 z" fill="var(--dv-py)"><title>Python server — 1.2x mongod</title></path><text x="229.1" y="485" class="dv-val">1.2<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 66.7 ms | 76.8 ms | 1.2× | 255.1 ms | 3.8× |
| find indexed range | 4.5 ms | 6.2 ms | 1.4× | 29.8 ms | 6.6× |
| find full scan | 7.8 ms | 17.4 ms | 2.2× | 59.0 ms | 7.6× |
| find filtered scan | 5.8 ms | 9.1 ms | 1.6× | 68.5 ms | 11.9× |
| update_many (half) | 35.0 ms | 37.3 ms | 1.1× | 448.8 ms | 12.8× |
| aggregate `$group` | 5.4 ms | 5.0 ms | 0.9× | 91.7 ms | 17.1× |
| aggregate multi-stage | 5.8 ms | 9.3 ms | 1.6× | 136.5 ms | 23.7× |
| delete_many (half) | 20.3 ms | 19.1 ms | 0.9× | 244.8 ms | 12.1× |
| change-stream drain\* | 51.0 ms | 33.3 ms | 0.7× | 61.8 ms | 1.2× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~0.7×–2.2× of mongod** per operation —
  three rows now beat mongod outright: delete_many (0.9×), single-op
  `aggregate $group` (0.9×), and the **change-stream drain (0.7× — faster
  than mongod at its own change streams**, after the reply path stopped
  re-encoding event blobs). update_many stays near parity (1.1×),
  the compound effect of the mimalloc allocator, link-time optimization,
  and profile-guided optimization on top of the raw-BSON write path and
  RecordId keying. The insert row reads 1.2× this cycle (0.8× on the
  2026-07-26 baseline) — bisected to a **cost shift, not a hot-path
  regression**: SecantusDB now creates its shard tables lazily on first
  write instead of eagerly at open (the change that took server open
  from ~500ms to milliseconds), and this benchmark times a fresh store's
  first-ever writes, so the insert number absorbs those one-time table
  creates. A warmed store inserts at the previous level, and sustained
  write *throughput* is up ~2.6× at eight writers (see
  [concurrency](concurrency.md)). The larger gaps are the read-scan /
  multi-stage-aggregate paths, dispatch and operator work above a
  storage engine that is literally the same C library.
- **The Python server runs at ~1.2×–24× of mongod** on these workloads —
  the 1.2× is the change-stream drain, where the work is oplog reads rather
  than per-document compute — and the Rust server is correspondingly
  **~1.9×–18× faster than the Python server** workload-for-workload (largest on update-heavy and aggregation
  paths, where Python does the most per-document work and where the Rust
  allocator + LTO win is biggest). The Python figures are stable against
  the previous baseline within noise.
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

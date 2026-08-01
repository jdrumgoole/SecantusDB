# Benchmark: both servers vs mongod

Generated 2026-07-30 on Darwin arm64 (Apple Silicon), `bench.compare_servers`.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 542" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="514" class="dv-grid"/><text x="295.2" y="530" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="514" class="dv-grid"/><text x="390.4" y="530" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="514" class="dv-grid"/><text x="485.6" y="530" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="514" class="dv-grid"/><text x="580.8" y="530" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="514" class="dv-grid"/><text x="676.0" y="530" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="514" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h18.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-18.9 z" fill="var(--dv-rust)"><title>Rust server — 1.2x mongod</title></path><text x="228.9" y="37" class="dv-val">1.2<tspan class="dv-x">x</tspan></text><path d="M200,42 h85.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-85.5 z" fill="var(--dv-py)"><title>Python server — 4.7x mongod</title></path><text x="295.5" y="53" class="dv-val">4.7<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h24.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-24.6 z" fill="var(--dv-rust)"><title>Rust server — 1.5x mongod</title></path><text x="234.6" y="91" class="dv-val">1.5<tspan class="dv-x">x</tspan></text><path d="M200,96 h135.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-135.0 z" fill="var(--dv-py)"><title>Python server — 7.3x mongod</title></path><text x="345.0" y="107" class="dv-val">7.3<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h39.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-39.8 z" fill="var(--dv-rust)"><title>Rust server — 2.3x mongod</title></path><text x="249.8" y="145" class="dv-val">2.3<tspan class="dv-x">x</tspan></text><path d="M200,150 h154.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-154.1 z" fill="var(--dv-py)"><title>Python server — 8.3x mongod</title></path><text x="364.1" y="161" class="dv-val">8.3<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">find filtered scan</text><path d="M200,188 h28.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-28.4 z" fill="var(--dv-rust)"><title>Rust server — 1.7x mongod</title></path><text x="238.4" y="199" class="dv-val">1.7<tspan class="dv-x">x</tspan></text><path d="M200,204 h239.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-239.8 z" fill="var(--dv-py)"><title>Python server — 12.8x mongod</title></path><text x="449.8" y="215" class="dv-val">12.8<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,242 h17.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-17.0 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="227.0" y="253" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,258 h249.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-249.3 z" fill="var(--dv-py)"><title>Python server — 13.3x mongod</title></path><text x="459.3" y="269" class="dv-val">13.3<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,296 h13.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-13.1 z" fill="var(--dv-rust)"><title>Rust server — 0.9x mongod</title></path><text x="223.1" y="307" class="dv-val">0.9<tspan class="dv-x">x</tspan></text><path d="M200,312 h327.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-327.4 z" fill="var(--dv-py)"><title>Python server — 17.4x mongod</title></path><text x="537.4" y="323" class="dv-val">17.4<tspan class="dv-x">x</tspan></text><text x="190" y="366" text-anchor="end" class="dv-lab">aggregate multi-stage</text><path d="M200,350 h28.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-28.4 z" fill="var(--dv-rust)"><title>Rust server — 1.7x mongod</title></path><text x="238.4" y="361" class="dv-val">1.7<tspan class="dv-x">x</tspan></text><path d="M200,366 h458.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-458.8 z" fill="var(--dv-py)"><title>Python server — 24.3x mongod</title></path><text x="668.8" y="377" class="dv-val">24.3<tspan class="dv-x">x</tspan></text><text x="190" y="420" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,404 h11.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-11.2 z" fill="var(--dv-rust)"><title>Rust server — 0.8x mongod</title></path><text x="221.2" y="415" class="dv-val">0.8<tspan class="dv-x">x</tspan></text><path d="M200,420 h196.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-196.0 z" fill="var(--dv-py)"><title>Python server — 10.5x mongod</title></path><text x="406.0" y="431" class="dv-val">10.5<tspan class="dv-x">x</tspan></text><text x="190" y="474" text-anchor="end" class="dv-lab">change-stream drain</text><path d="M200,458 h11.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-11.2 z" fill="var(--dv-rust)"><title>Rust server — 0.8x mongod</title></path><text x="221.2" y="469" class="dv-val">0.8<tspan class="dv-x">x</tspan></text><path d="M200,474 h22.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-22.7 z" fill="var(--dv-py)"><title>Python server — 1.4x mongod</title></path><text x="232.7" y="485" class="dv-val">1.4<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 55.0 ms | 65.8 ms | 1.2× | 256.7 ms | 4.7× |
| find indexed range | 4.3 ms | 6.3 ms | 1.5× | 31.5 ms | 7.3× |
| find full scan | 7.3 ms | 16.7 ms | 2.3× | 60.4 ms | 8.3× |
| find filtered scan | 5.5 ms | 9.2 ms | 1.7× | 70.2 ms | 12.8× |
| update_many (half) | 33.4 ms | 36.8 ms | 1.1× | 445.9 ms | 13.3× |
| aggregate `$group` | 5.5 ms | 5.1 ms | 0.9× | 95.3 ms | 17.4× |
| aggregate multi-stage | 5.8 ms | 9.9 ms | 1.7× | 142.0 ms | 24.3× |
| delete_many (half) | 20.9 ms | 17.6 ms | 0.8× | 218.2 ms | 10.5× |
| change-stream drain\* | 45.8 ms | 36.9 ms | 0.8× | 64.3 ms | 1.4× |

\* Change-stream drain: 5,000 events consumed through a `watch()` cursor
(only the drain is timed). mongod's number is measured against a throwaway
**single-node replica set** — its change streams require one — while every
other row keeps the standalone-mongod reference, so the rest of the table
stays comparable with earlier publications.

## Reading the numbers

- **The Rust server runs at ~0.8×–2.3× of mongod** per operation —
  three rows now beat mongod outright: delete_many (0.8×), single-op
  `aggregate $group` (0.9×), and the **change-stream drain (0.8× — faster
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
- **The Python server runs at ~1.4×–24× of mongod** on these workloads —
  the 1.4× is the change-stream drain, where the work is oplog reads rather
  than per-document compute — and the Rust server is correspondingly
  **~1.7×–19× faster than the Python server** workload-for-workload (largest on update-heavy and aggregation
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

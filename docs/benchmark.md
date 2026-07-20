# Benchmark: both servers vs mongod

Generated 2026-07-17 on Darwin arm64 (Apple Silicon), `bench.compare_servers`.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 380" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="352" class="dv-grid"/><text x="295.2" y="368" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="352" class="dv-grid"/><text x="390.4" y="368" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="352" class="dv-grid"/><text x="485.6" y="368" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="352" class="dv-grid"/><text x="580.8" y="368" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="352" class="dv-grid"/><text x="676.0" y="368" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="352" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h26.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.5 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236" y="37" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,42 h104.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-104.5 z" fill="var(--dv-py)"><title>Python server — 5.7x mongod</title></path><text x="315" y="53" class="dv-val">5.7<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h24.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-24.6 z" fill="var(--dv-rust)"><title>Rust server — 1.5x mongod</title></path><text x="235" y="91" class="dv-val">1.5<tspan class="dv-x">x</tspan></text><path d="M200,96 h121.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-121.7 z" fill="var(--dv-py)"><title>Python server — 6.6x mongod</title></path><text x="332" y="107" class="dv-val">6.6<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h43.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-43.6 z" fill="var(--dv-rust)"><title>Rust server — 2.5x mongod</title></path><text x="254" y="145" class="dv-val">2.5<tspan class="dv-x">x</tspan></text><path d="M200,150 h222.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-222.6 z" fill="var(--dv-py)"><title>Python server — 11.9x mongod</title></path><text x="433" y="161" class="dv-val">11.9<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,188 h24.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-24.6 z" fill="var(--dv-rust)"><title>Rust server — 1.5x mongod</title></path><text x="235" y="199" class="dv-val">1.5<tspan class="dv-x">x</tspan></text><path d="M200,204 h254.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-254.9 z" fill="var(--dv-py)"><title>Python server — 13.6x mongod</title></path><text x="465" y="215" class="dv-val">13.6<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,242 h39.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-39.8 z" fill="var(--dv-rust)"><title>Rust server — 2.3x mongod</title></path><text x="250" y="253" class="dv-val">2.3<tspan class="dv-x">x</tspan></text><path d="M200,258 h437.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-437.7 z" fill="var(--dv-py)"><title>Python server — 23.2x mongod</title></path><text x="648" y="269" class="dv-val">23.2<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,296 h26.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.5 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236" y="307" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,312 h317.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-317.8 z" fill="var(--dv-py)"><title>Python server — 16.9x mongod</title></path><text x="528" y="323" class="dv-val">16.9<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 57.6 ms | 91.0 ms | 1.6× | 328.4 ms | 5.7× |
| find indexed range | 4.4 ms | 6.5 ms | 1.5× | 29.2 ms | 6.6× |
| find full scan | 7.8 ms | 19.6 ms | 2.5× | 93.2 ms | 11.9× |
| update_many (half) | 35.4 ms | 52.4 ms | 1.5× | 480.5 ms | 13.6× |
| aggregate `$group` | 5.7 ms | 13.0 ms | 2.3× | 132.8 ms | 23.2× |
| delete_many (half) | 20.7 ms | 34.1 ms | 1.6× | 351.1 ms | 16.9× |

## Reading the numbers

- **The Rust server runs at 1.5×–2.5× of mongod** per operation — the gap
  that remains is dispatch and operator work above a storage engine that is
  literally the same C library.
- **The Python server runs at 6×–23× of mongod** on these workloads, and
  the Rust server is correspondingly **~4×–10× faster than the Python
  server** workload-for-workload (largest on update-heavy and aggregation
  paths, where Python does the most per-document work).
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

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 380" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="319" y1="18" x2="319" y2="352" class="dv-grid"/><text x="319" y="368" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="438" y1="18" x2="438" y2="352" class="dv-grid"/><text x="438" y="368" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="557" y1="18" x2="557" y2="352" class="dv-grid"/><text x="557" y="368" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="676" y1="18" x2="676" y2="352" class="dv-grid"/><text x="676" y="368" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="224" y1="18" x2="224" y2="352" class="dv-ref"/><text x="224" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h46.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-46.0 z" fill="var(--dv-rust)"><title>Rust server — 2.1x mongod</title></path><text x="256" y="37" class="dv-val">2.1<tspan class="dv-x">x</tspan></text><path d="M200,42 h138.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-138.9 z" fill="var(--dv-py)"><title>Python server — 6.0x mongod</title></path><text x="349" y="53" class="dv-val">6.0<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h53.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-53.1 z" fill="var(--dv-rust)"><title>Rust server — 2.4x mongod</title></path><text x="263" y="91" class="dv-val">2.4<tspan class="dv-x">x</tspan></text><path d="M200,96 h155.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-155.5 z" fill="var(--dv-py)"><title>Python server — 6.7x mongod</title></path><text x="366" y="107" class="dv-val">6.7<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h103.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-103.1 z" fill="var(--dv-rust)"><title>Rust server — 4.5x mongod</title></path><text x="313" y="145" class="dv-val">4.5<tspan class="dv-x">x</tspan></text><path d="M200,150 h286.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-286.5 z" fill="var(--dv-py)"><title>Python server — 12.2x mongod</title></path><text x="496" y="161" class="dv-val">12.2<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,188 h57.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-57.9 z" fill="var(--dv-rust)"><title>Rust server — 2.6x mongod</title></path><text x="268" y="199" class="dv-val">2.6<tspan class="dv-x">x</tspan></text><path d="M200,204 h324.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-324.6 z" fill="var(--dv-py)"><title>Python server — 13.8x mongod</title></path><text x="535" y="215" class="dv-val">13.8<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,242 h98.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-98.4 z" fill="var(--dv-rust)"><title>Rust server — 4.3x mongod</title></path><text x="308" y="253" class="dv-val">4.3<tspan class="dv-x">x</tspan></text><path d="M200,258 h484.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-484.1 z" fill="var(--dv-py)"><title>Python server — 20.5x mongod</title></path><text x="694" y="269" class="dv-val">20.5<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,296 h84.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-84.1 z" fill="var(--dv-rust)"><title>Rust server — 3.7x mongod</title></path><text x="294" y="307" class="dv-val">3.7<tspan class="dv-x">x</tspan></text><path d="M200,312 h346.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-346.0 z" fill="var(--dv-py)"><title>Python server — 14.7x mongod</title></path><text x="556" y="323" class="dv-val">14.7<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 55.6 ms | 118.8 ms | 2.1× | 332.5 ms | 6.0× |
| find indexed range | 4.3 ms | 10.1 ms | 2.4× | 28.6 ms | 6.7× |
| find full scan | 7.7 ms | 34.8 ms | 4.5× | 93.5 ms | 12.2× |
| update_many (half) | 35.2 ms | 93.0 ms | 2.6× | 486.9 ms | 13.8× |
| aggregate `$group` | 5.8 ms | 24.6 ms | 4.3× | 118.6 ms | 20.5× |
| delete_many (half) | 21.6 ms | 80.5 ms | 3.7× | 317.1 ms | 14.7× |

## Reading the numbers

- **The Rust server runs at 2.1×–4.5× of mongod** per operation — the gap
  that remains is dispatch and operator work above a storage engine that is
  literally the same C library.
- **The Python server runs at 6×–20.5× of mongod** on these workloads, and
  the Rust server is correspondingly **~2.7×–5.2× faster than the Python
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

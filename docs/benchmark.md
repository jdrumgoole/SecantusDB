# Benchmark: both servers vs mongod

Generated 2026-07-26 on Darwin arm64 (Apple Silicon), `bench.compare_servers`.

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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 380" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="352" class="dv-grid"/><text x="295.2" y="368" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="352" class="dv-grid"/><text x="390.4" y="368" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="352" class="dv-grid"/><text x="485.6" y="368" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="352" class="dv-grid"/><text x="580.8" y="368" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="352" class="dv-grid"/><text x="676.0" y="368" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="352" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h11.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-11.2 z" fill="var(--dv-rust)"><title>Rust server — 0.8x mongod</title></path><text x="221" y="37" class="dv-val">0.8<tspan class="dv-x">x</tspan></text><path d="M200,42 h77.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-77.9 z" fill="var(--dv-py)"><title>Python server — 4.3x mongod</title></path><text x="288" y="53" class="dv-val">4.3<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h22.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-22.7 z" fill="var(--dv-rust)"><title>Rust server — 1.4x mongod</title></path><text x="233" y="91" class="dv-val">1.4<tspan class="dv-x">x</tspan></text><path d="M200,96 h127.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-127.4 z" fill="var(--dv-py)"><title>Python server — 6.9x mongod</title></path><text x="337" y="107" class="dv-val">6.9<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h36.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-36.0 z" fill="var(--dv-rust)"><title>Rust server — 2.1x mongod</title></path><text x="246" y="145" class="dv-val">2.1<tspan class="dv-x">x</tspan></text><path d="M200,150 h144.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-144.5 z" fill="var(--dv-py)"><title>Python server — 7.8x mongod</title></path><text x="354" y="161" class="dv-val">7.8<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,188 h11.2 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-11.2 z" fill="var(--dv-rust)"><title>Rust server — 0.8x mongod</title></path><text x="221" y="199" class="dv-val">0.8<tspan class="dv-x">x</tspan></text><path d="M200,204 h235.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-235.9 z" fill="var(--dv-py)"><title>Python server — 12.6x mongod</title></path><text x="446" y="215" class="dv-val">12.6<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,242 h16.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-16.9 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="227" y="253" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,258 h319.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-319.7 z" fill="var(--dv-py)"><title>Python server — 17.0x mongod</title></path><text x="530" y="269" class="dv-val">17.0<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,296 h13.1 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-13.1 z" fill="var(--dv-rust)"><title>Rust server — 0.9x mongod</title></path><text x="223" y="307" class="dv-val">0.9<tspan class="dv-x">x</tspan></text><path d="M200,312 h199.7 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-199.7 z" fill="var(--dv-py)"><title>Python server — 10.7x mongod</title></path><text x="410" y="323" class="dv-val">10.7<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 58.8 ms | 46.5 ms | 0.8× | 252.2 ms | 4.3× |
| find indexed range | 4.4 ms | 6.1 ms | 1.4× | 30.7 ms | 6.9× |
| find full scan | 7.7 ms | 16.5 ms | 2.1× | 60.2 ms | 7.8× |
| update_many (half) | 35.6 ms | 28.2 ms | 0.8× | 446.8 ms | 12.6× |
| aggregate `$group` | 5.8 ms | 6.2 ms | 1.1× | 98.0 ms | 17.0× |
| delete_many (half) | 20.7 ms | 18.1 ms | 0.9× | 221.1 ms | 10.7× |

## Reading the numbers

- **The Rust server runs at ~0.8×–2.1× of mongod** per operation — writes now
  beat standalone `mongod` (insert 0.8×, update 0.8×, delete 0.9×) and single-op
  `aggregate $group` is at parity (1.1×), after the mimalloc allocator,
  link-time optimization, and profile-guided optimization cut the
  BSON-materialization allocation and hot-path branch cost that dominated the
  write and aggregate paths (on top of the earlier oplog write-path fix that
  stopped re-encoding documents it already had); the larger gaps are the
  read-scan / multi-stage-aggregate paths, dispatch and operator work above a
  storage engine that is literally the same C library.
- **The Python server runs at ~4×–17× of mongod** on these workloads, and
  the Rust server is correspondingly **~4×–14× faster than the Python
  server** workload-for-workload (largest on update-heavy and aggregation
  paths, where Python does the most per-document work and where the Rust
  allocator + LTO win is biggest). The Python scan and aggregation figures
  improved markedly (full scan 12.0× → 7.8×, `$group` 22.4× → 17.0×, insert
  4.9× → 4.3×) when the Python document table became keyed by RecordId: an
  unsorted walk now reads the table directly instead of going through a side
  index, and an insert writes three rows instead of four. The same change
  costs a little on the indexed-read and update paths (6.5× → 6.9×,
  12.1× → 12.6×), which now unpack the document's key out of the stored row.
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

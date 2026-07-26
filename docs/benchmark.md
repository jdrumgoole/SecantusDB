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
</style><div class="dviz-wrap"><div class="dv-legend"><span><span class="chip" style="background:var(--dv-rust)"></span>Rust server</span><span><span class="chip" style="background:var(--dv-py)"></span>Python server</span></div><svg viewBox="0 0 760 380" role="img" aria-label="Per-operation latency as a multiple of mongod" class="dviz"><line x1="295.2" y1="18" x2="295.2" y2="352" class="dv-grid"/><text x="295.2" y="368" text-anchor="middle" class="dv-tick">5<tspan class="dv-x">x</tspan></text><line x1="390.4" y1="18" x2="390.4" y2="352" class="dv-grid"/><text x="390.4" y="368" text-anchor="middle" class="dv-tick">10<tspan class="dv-x">x</tspan></text><line x1="485.6" y1="18" x2="485.6" y2="352" class="dv-grid"/><text x="485.6" y="368" text-anchor="middle" class="dv-tick">15<tspan class="dv-x">x</tspan></text><line x1="580.8" y1="18" x2="580.8" y2="352" class="dv-grid"/><text x="580.8" y="368" text-anchor="middle" class="dv-tick">20<tspan class="dv-x">x</tspan></text><line x1="676.0" y1="18" x2="676.0" y2="352" class="dv-grid"/><text x="676.0" y="368" text-anchor="middle" class="dv-tick">25<tspan class="dv-x">x</tspan></text><line x1="219.0" y1="18" x2="219.0" y2="352" class="dv-ref"/><text x="219.0" y="12" text-anchor="middle" class="dv-tick">mongod = 1<tspan class="dv-x">x</tspan></text><text x="190" y="42" text-anchor="end" class="dv-lab">insert (10k docs)</text><path d="M200,26 h16.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-16.9 z" fill="var(--dv-rust)"><title>Rust server — 1.1x mongod</title></path><text x="227" y="37" class="dv-val">1.1<tspan class="dv-x">x</tspan></text><path d="M200,42 h77.9 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-77.9 z" fill="var(--dv-py)"><title>Python server — 4.3x mongod</title></path><text x="288" y="53" class="dv-val">4.3<tspan class="dv-x">x</tspan></text><text x="190" y="96" text-anchor="end" class="dv-lab">find indexed range</text><path d="M200,80 h26.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-26.5 z" fill="var(--dv-rust)"><title>Rust server — 1.6x mongod</title></path><text x="236" y="91" class="dv-val">1.6<tspan class="dv-x">x</tspan></text><path d="M200,96 h129.3 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-129.3 z" fill="var(--dv-py)"><title>Python server — 7.0x mongod</title></path><text x="339" y="107" class="dv-val">7.0<tspan class="dv-x">x</tspan></text><text x="190" y="150" text-anchor="end" class="dv-lab">find full scan</text><path d="M200,134 h36.0 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-36.0 z" fill="var(--dv-rust)"><title>Rust server — 2.1x mongod</title></path><text x="246" y="145" class="dv-val">2.1<tspan class="dv-x">x</tspan></text><path d="M200,150 h142.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-142.6 z" fill="var(--dv-py)"><title>Python server — 7.7x mongod</title></path><text x="353" y="161" class="dv-val">7.7<tspan class="dv-x">x</tspan></text><text x="190" y="204" text-anchor="end" class="dv-lab">update_many (half)</text><path d="M200,188 h20.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-20.8 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231" y="199" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,204 h237.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-237.8 z" fill="var(--dv-py)"><title>Python server — 12.7x mongod</title></path><text x="448" y="215" class="dv-val">12.7<tspan class="dv-x">x</tspan></text><text x="190" y="258" text-anchor="end" class="dv-lab">aggregate $group</text><path d="M200,242 h28.4 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-28.4 z" fill="var(--dv-rust)"><title>Rust server — 1.7x mongod</title></path><text x="238" y="253" class="dv-val">1.7<tspan class="dv-x">x</tspan></text><path d="M200,258 h323.5 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-323.5 z" fill="var(--dv-py)"><title>Python server — 17.2x mongod</title></path><text x="533" y="269" class="dv-val">17.2<tspan class="dv-x">x</tspan></text><text x="190" y="312" text-anchor="end" class="dv-lab">delete_many (half)</text><path d="M200,296 h20.8 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-20.8 z" fill="var(--dv-rust)"><title>Rust server — 1.3x mongod</title></path><text x="231" y="307" class="dv-val">1.3<tspan class="dv-x">x</tspan></text><path d="M200,312 h201.6 a4.0,4.0 0 0 1 4.0,4.0 v6.0 a4.0,4.0 0 0 1 -4.0,4.0 h-201.6 z" fill="var(--dv-py)"><title>Python server — 10.8x mongod</title></path><text x="412" y="323" class="dv-val">10.8<tspan class="dv-x">x</tspan></text></svg></div>
```

| Workload | mongod | Rust server | ×mongod | Python server | ×mongod |
|---|---:|---:|---:|---:|---:|
| insert (10k docs) | 59.5 ms | 64.8 ms | 1.1× | 257.0 ms | 4.3× |
| find indexed range | 4.5 ms | 7.0 ms | 1.6× | 31.3 ms | 7.0× |
| find full scan | 7.9 ms | 16.5 ms | 2.1× | 60.3 ms | 7.7× |
| update_many (half) | 35.7 ms | 46.8 ms | 1.3× | 451.8 ms | 12.7× |
| aggregate `$group` | 5.8 ms | 10.0 ms | 1.7× | 99.7 ms | 17.2× |
| delete_many (half) | 20.6 ms | 26.8 ms | 1.3× | 222.1 ms | 10.8× |

## Reading the numbers

- **The Rust server runs at 1.1×–2.1× of mongod** per operation — writes are
  now within ~10–30% of mongod (insert 1.1×, update / delete 1.3×) after the
  oplog write path stopped re-encoding documents it already had; the larger
  gaps are the read/aggregate paths, dispatch and operator work above a storage
  engine that is literally the same C library.
- **The Python server runs at ~4×–17× of mongod** on these workloads, and
  the Rust server is correspondingly **~4×–10× faster than the Python
  server** workload-for-workload (largest on update-heavy and aggregation
  paths, where Python does the most per-document work). The scan and
  aggregation figures improved markedly (full scan 12.0× → 7.7×, `$group`
  22.4× → 17.2×, insert 4.9× → 4.3×) when the Python document table became
  keyed by RecordId: an unsorted walk now reads the table directly instead of
  going through a side index, and an insert writes three rows instead of four.
  The same change costs a little on the indexed-read and update paths
  (6.5× → 7.0×, 12.1× → 12.7×), which now unpack the document's key out of the
  stored row.
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

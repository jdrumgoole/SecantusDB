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

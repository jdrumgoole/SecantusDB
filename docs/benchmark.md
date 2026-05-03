# SecantusDB vs mongod benchmark

Generated 2026-05-03 on Darwin arm64 (arm).

Both servers use the **same WiredTiger storage engine** — mongod ships it; SecantusDB vendors the same C library — driven by the same `pymongo` client over the wire protocol. The hot path differs only above the storage layer (command dispatch, query planner, aggregation pipeline), so this is a fair comparison of the parts of SecantusDB that aren't WiredTiger itself.

Each workload runs against a freshly-spawned daemon on a free port with its own tmp data dir, both on-disk WiredTiger. Each timed 5× per server; we report median + p95 in milliseconds. Dataset is 10,000 small docs.

## Results

| Workload | SecantusDB median | SecantusDB p95 | mongod median | mongod p95 | sec/mongod |
|---|---:|---:|---:|---:|---:|
| insert_many (10k docs) | 414.2 ms | 487.5 ms | 46.6 ms | 47.2 ms | 8.89× |
| find indexed eq (20×500 docs) | 166.1 ms | 187.0 ms | 20.7 ms | 20.8 ms | 8.02× |
| find indexed range ($gte/$lt) | 95.0 ms | 100.9 ms | 11.7 ms | 14.8 ms | 8.15× |
| aggregate $group + $sort | 223.0 ms | 247.4 ms | 5.8 ms | 7.4 ms | 38.41× |
| update_many ($inc on 5k docs) | 862.1 ms | 1046.3 ms | 21.6 ms | 24.2 ms | 39.92× |
| delete_many (5k docs by range) | 484.6 ms | 549.4 ms | 10.6 ms | 11.3 ms | 45.80× |

## How to read this

**`sec/mongod` < 1.0×** = SecantusDB is faster on this workload. **> 1.0×** = mongod is faster, by that ratio. 1.0× = parity.

Both servers are running real WiredTiger on the same machine against the same dataset. Differences come from the layers above WT — SecantusDB's Python command dispatch and pure-Python query planner / aggregation pipeline vs mongod's compiled C++ versions.

The numbers above are **single-machine, single-process, no concurrency** — a deliberately narrow scenario to isolate per-operation latency. Throughput under concurrent connections is a separate measurement (and a place where mongod's connection pooling / async accept loop is going to win regardless).

## How to refresh

```bash
uv run --no-sync python -m bench.compare_mongod
```

Requires `mongod` on `PATH` (Community Server is enough). On macOS: `brew tap mongodb/brew && brew install mongodb-community`. On Linux: see https://www.mongodb.com/docs/manual/installation/.

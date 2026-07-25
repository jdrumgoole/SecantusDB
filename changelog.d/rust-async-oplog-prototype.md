### Rust server: an opt-in async oplog that more than doubles multi-writer throughput (prototype)

Profiling established that the shared oplog every write appends is the *sole*
concurrency ceiling on the Rust server: with the oplog off it scales ~4.8× at eight
concurrent writers (matching a real `mongod`), but writing each entry inside the
write's own transaction makes concurrent writers contend on the shared oplog btrees
and WiredTiger's WAL, collapsing eight-writer scaling to ~1.8×. This ships a working,
opt-in prototype that takes the oplog write off the writer's critical path: a
committed write's entries are minted a sequence number and handed to a background
drainer thread that persists them in order, while change-stream tailers wait on the
drainer's durable watermark. On eight writers this measured **2.2× the synchronous
throughput** (46.7k → 102.8k docs/s), lifting scaling from 1.8× to **4.0×** —
approaching mongod's 4.67×.

It is **off by default** (`SECANTUS_OPLOG_ASYNC=1` to enable) because it changes a
durability property: the oplog is no longer atomic with the data and a hard crash
loses entries the drainer had not yet written (data stays fully durable; a clean
shutdown flushes the drainer before checkpointing, so a clean restart preserves the
whole oplog). Correctness under concurrency is validated — six parallel writers doing
3000 inserts, and a cluster-wide change stream observes every insert exactly once,
with zero duplicates or misses. See `tasks/rust-async-oplog-prototype.md` for the
design and the remaining productionization work (backpressure, a real config knob,
read-after-write semantics).

#### Added
- `SECANTUS_OPLOG_ASYNC=1` (Rust server, opt-in, prototype): oplog entries are
  persisted by a background drainer off the writer's transaction. `Storage::flush_oplog`
  blocks until the drainer has caught up (read-after-write oplog visibility).

#### Changed
- The Rust `Storage` holds its `Connection` / oplog state behind `Arc` so the drainer
  thread can share them; `wait_for_oplog` blocks on the drainer's `written_seq`
  watermark in async mode and on `next_seq - 1` (unchanged) in the synchronous
  default.

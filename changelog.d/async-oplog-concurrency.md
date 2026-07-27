### Rust server: 2.2× multi-writer throughput with the async + non-logged oplog stack

The Rust server's concurrent-write ceiling was the oplog: every write paid a
WAL-logged oplog append, holding 8-writer throughput to ~56k docs/s (sync
default) or ~88k with the async-oplog prototype, against a ~191k no-oplog
ceiling. Two new opt-in levers close most of that gap. Setting
`SECANTUS_OPLOG_NONLOGGED=1` creates the oplog and pre-image tables with
WiredTiger WAL logging disabled, so oplog rows are checkpoint-durable only — in
async mode that removes the drainer's WAL volume from the writers' path and
lifts 8-writer throughput to ~125k docs/s, 2.2× the sync default (and ~1.9× a
single writer's async rate), while change streams stay exactly-once. The async
drainer also now coalesces queued batches into one WiredTiger transaction (up
to 32 batches / 16 MB; `SECANTUS_OPLOG_ASYNC_COALESCE=0` disables it). The
durability trade is explicit and opt-in: a hard crash loses the oplog tail
written since the last checkpoint (data tables stay fully logged and durable; a
clean shutdown flushes and checkpoints a complete oplog). Defaults are
unchanged — the synchronous, fully-logged oplog remains the out-of-the-box
behaviour.

#### Added

- `SECANTUS_OPLOG_NONLOGGED=1` — create the oplog + preimage tables with
  `log=(enabled=false)` (checkpoint-durable oplog; data unaffected). Applies at
  table-create time on a fresh store.
- Async-oplog drainer batch coalescing: queued `DrainBatch`es are written in a
  single WT transaction (caps: 32 batches / 16 MB), on by default in async
  mode; `SECANTUS_OPLOG_ASYNC_COALESCE=0` restores per-batch commits.
- `SECANTUS_DISABLE_OPLOG=1` on `secantusd-rs` — run the daemon with oplog
  emission off entirely (the "drop the oplog" throughput lever from
  `docs/concurrency.md`, previously reachable only via the embedded API).

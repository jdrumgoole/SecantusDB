### The Rust PostgreSQL server group-commits its journal writes

Concurrent INSERT throughput on the Rust PostgreSQL server in durable mode
stopped growing at about 2x with more clients, where PostgreSQL itself reached
about 4x. Two things kept every commit waiting on its own synced journal write.
The background oplog pruner deleted old entries one autocommit at a time, so
each delete was a synced log write of its own, competing with the commits
clients were waiting on. And WiredTiger closes a log slot on every synced
commit, so concurrent commits never shared a write.

The pruner now deletes in transactions of 1,024 rows. The PostgreSQL server
now commits a user transaction without the per-commit sync, then waits on a
shared log flush. One flush runs at a time and covers every commit made before
it started, which is PostgreSQL's group commit. A commit is still acknowledged
only after its log record has been written through the same synced path, and
a new test SIGKILLs the server under eight concurrent writers and checks every
acknowledged row. Measured at 8 clients: 24.4k to 33.2k inserts/s. FROM-less
literal SELECTs (`select 1`) also now run on the connection's worker thread
instead of being handed off, which saves about 2 µs a statement.

#### Changed

- `secantus-storage`: `Storage::set_group_commit` turns on leader/follower
  group commit for user transactions (`commit sync=off` + one shared
  `log_flush`). The Rust MongoDB server never calls it and is unchanged.
- `secantus-storage`: the oplog prune sweep deletes in transactions of 1,024
  rows instead of one autocommit per row.
- `secantus-wt`: `Session::log_flush`.
- `secantus-pgserver`: durable mode turns group commit on (`sync=off` flush on
  macOS, where the log is `O_DSYNC`; `sync=on` elsewhere). Literal-only
  FROM-less SELECTs skip the `block_in_place` hand-off.

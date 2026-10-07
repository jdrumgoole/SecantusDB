### The oplog pruner stops monopolising the Rust PostgreSQL server's journal

In durable mode, concurrent INSERT throughput on the Rust PostgreSQL server
stopped growing at about 2x with more clients. Most of that ceiling came from
the background oplog pruner. It deleted old entries one autocommit at a time,
so every delete was a synced journal write of its own, competing with the
commits clients were waiting on. It now deletes in transactions of 1,024 rows.
Measured at 8 clients, release build: 24.4k to 27.5k inserts/s, with 2 clients
at 1.95x the single-client rate (was 1.55x).

FROM-less literal SELECTs (`select 1`) now run on the connection's worker
thread instead of being handed off to another one, which saves about 2 µs a
statement.

A new test SIGKILLs the server under eight concurrent writers and a reader.
Every row a writer saw acknowledged, and every row the reader saw, must
survive the restart. As in PostgreSQL, a committed row is visible to other
sessions only once it is durable.

#### Changed

- `secantus-storage`: the oplog prune sweep deletes in transactions of 1,024
  rows instead of one autocommit per row. The MongoDB server benefits the
  same way under `--sync-on-commit`.
- `secantus-pgserver`: literal-only FROM-less SELECTs skip the
  `block_in_place` hand-off.

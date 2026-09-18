### Where the Rust PostgreSQL server's statement time actually goes

The previous release established that this server costs roughly five times more
CPU per statement than PostgreSQL, and that no lock was responsible. It could
not say which layer owned the time. It can now, and the answer is that almost
none of it belongs to the query.

A statement that touches no table at all — `select 1` — costs fifty-five
microseconds more than a bare protocol round trip, where PostgreSQL pays nine.
The wire layer is not to blame: an empty round trip is within two microseconds
of PostgreSQL's. Reading one row by primary key adds only twelve more, so
storage is not to blame either. The overhead sits in catalog bookkeeping done
once per statement, by two different mechanisms. Outside a transaction, every
statement opens seven fresh WiredTiger sessions to re-confirm that catalog
collections which are created once and never dropped still exist. Inside a
transaction block it is worse rather than better — around forty microseconds
worse — because holding a block makes the catalog cache ineligible and each
statement re-scans the catalog from storage instead.

One finding from this work has been retracted rather than shipped. A single run
suggested prepared statements cost seventy-seven microseconds more than
unprepared ones, which would have been a serious and surprising defect; four
further runs put the two within noise of each other. The number was an
artifact, and the backlog now says so explicitly, because a plausible phantom
that nobody can reproduce is more expensive than no finding at all.

#### Added

- `bench/pg_statement_cost.py`: a layer bisect from a bare protocol round trip
  up to a prepared row read, run against both servers, with `--in-transaction`
  for the block path.
- `tests/test_pg_statement_cost_bench.py`, pinning the stage set the
  attribution is a difference between.

#### Changed

- The backlog entry now carries the measured per-layer figures, the two call
  paths behind them, the retraction above, and the instruction to repeat a
  measurement before quoting it.

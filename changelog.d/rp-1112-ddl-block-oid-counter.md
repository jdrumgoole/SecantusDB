### DDL inside a transaction block costs the same at its last statement as at its first

A migration tool's block of `CREATE TABLE`s (Alembic, Django) ran in time
that grew with the square of its statements on the Rust PostgreSQL server:
twenty in one `BEGIN` ... `COMMIT`, or in one multi-statement query, took a
debug `secantusd-pg` 14.2 s where autocommit took 1.2 s. Twenty in one query
now take 0.73 s, and each statement in a block of thirty costs what the first
did.

#### Fixed

- Under READ COMMITTED a statement in a block that had written was moved onto
  a new snapshot whenever something had committed since its own, which
  replays every write the block has made. The oid counter commits outside the
  block for every relation, type or constraint a statement creates (as
  PostgreSQL's `GetNewObjectId` reads shared memory, not a snapshot), so each
  DDL statement after the first replayed the whole block, the WiredTiger
  creates of its tables among them. The staleness check now passes over the
  counter, which no statement reads through the block, as it already passed
  over a sequence the block had not written.
- Fixes RP-1112 (`tests/backlog/test_rp_catalog_latency.py::test_ten_create_tables_in_one_block_cost_what_they_do_one_at_a_time`).

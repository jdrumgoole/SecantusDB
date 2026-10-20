### Rust PostgreSQL server: a hash join too big for work_mem runs in batches, as PostgreSQL's does

When PostgreSQL's hash table would not fit in `work_mem` x
`hash_mem_multiplier` (4 MB x 2), it splits the join into batches. That
costs page I/O, which can change the plan, and the rows come out batch by
batch: an outer row whose key hashes to a later batch is joined after every
row of the first. The join order search now models both, so a chain of five
tables whose top join hashes an estimated 212,242 rows returns PostgreSQL
15.19's rows in PostgreSQL's order.

#### Fixed

- `secantus-pgplan` (`joincost.rs`): `hash_table_size` transcribes
  `ExecChooseHashTableSize` (skew allowance, bucket and batch counts),
  `hash_join` adds the batches' page I/O (`initial_cost_hashjoin`) and
  uses every batch's buckets (`final_cost_hashjoin`), and `hash_batch` is
  `ExecHashGetBucketAndBatch`. A side's row width (`JoinSide::width`) sizes
  the table; a side whose width is not known is priced as one batch, as
  before.
- `secantus-pgplan` (`joins.rs`): a join source records the joined columns
  the query reads above it (`JoinRows::needed`). The join order search uses
  them, each clause's key to a table outside a relation, and the
  equivalence class's members while one is outside, as each relation's row
  width (`build_joinrel_tlist`).
- `secantus-pg` (`join_search.rs`, `join_order.rs`): a hash join split into
  batches returns its outer rows batch by batch. A hash RIGHT join split
  into batches is declined. Fixes part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_hash_join_too_big_for_work_mem_returns_rows_batch_by_batch`,
  now a regression test). Measured against PostgreSQL 15.19: a class over
  five tables 27 of 27 shapes at 20, 300 and 3,000 rows (21 before), every
  other four- to eight-table shape unchanged (432 of 432).
- `secantus-pgplan`: `read_apart::has_subquery` remembers its last answer,
  and `uuid_from_wire` writes its digits directly. Both ran on every
  execution of a prepared UUID insert (RP-014, part).

#### Added

- RP-028's open gap is now a two-table RIGHT join, which PostgreSQL plans
  as a merge left join and the general join path does not:
  `tests/backlog/test_rust_postgres_gaps.py::test_a_right_join_of_two_tables_is_planned_as_postgresql_plans_it`.
  RP-014 records the Linux measurement after #2032 and what bounds the
  insert rate now: storage's per-collection write lock.

### Rust PostgreSQL server: a join's order counts only while something above it can use it

A chain of five tables whose ON clauses share a column could come back in
the mirror image of PostgreSQL's row order. The join order search kept a
path sorted on an already-consumed join clause beside an equally cheap path
sorted on the shared column, and took the wrong one at a tie. PostgreSQL
drops an order that nothing above the join can use
(`truncate_useless_pathkeys`), and the search now does the same.

#### Fixed

- `secantus-pg` (`join_order.rs`), `secantus-pgplan` (`joincost.rs`): a
  merge join's order is kept only while a join clause still to come is on
  its key -- for the equivalence class, while a member is outside the
  relation; for any other clause, never once it is joined
  (`JoinRel::add_paths_on` with `USELESS_KEYS`). Fixes part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_five_tables_read_a_classes_sorted_join_under_a_sorted_outer`,
  now a regression test). Measured against PostgreSQL 15.19: a class over
  five tables 21 of 27 shapes (12 before), and every other shape of four to
  eight tables unchanged (432 of 432).

#### Added

- RP-028's remaining gap is a hash join too big for `work_mem`, which
  PostgreSQL runs in batches and which returns rows batch by batch:
  `tests/backlog/test_rust_postgres_gaps.py::test_a_hash_join_too_big_for_work_mem_returns_rows_batch_by_batch`.

### Rust PostgreSQL server: GROUP BY rows in PostgreSQL's order

A GROUP BY that PostgreSQL plans as a HashAggregate returns its groups in
the order of its hash table, and an ORDER BY above it leaves tied groups in
that order. The Rust PostgreSQL server returned them in the order each
group first appeared. It now reproduces the hash table: the first slice of
the cost model Joe decided on (2026-10-16, RP-028).

#### Changed

- **A GROUP BY of plain columns over one table comes out in PostgreSQL
  15's HashAggregate order**, and an ORDER BY over it breaks ties as
  PostgreSQL's quicksort does over that order. The planner's estimate of
  the groups for a table with no statistics, the tuple hash and
  simplehash's placement and iteration are transcribed from PostgreSQL 15
  (`secantus-pgplan/src/hashagg.rs`); 12 shapes match 15.19 exactly, among
  them a primary-key, a boolean, a NULL-bearing and a two-column key
  (tests/backlog/test_rust_postgres_gaps.py::test_grouped_output_ties_as_postgres_leaves_them,
  tests/backlog/test_rust_postgres_gaps.py::test_group_by_rows_come_out_in_hash_aggregate_order).
  A WHERE, an expression key, a join, `SELECT DISTINCT` and very large
  group counts keep the order they had; RP-028 lists them.

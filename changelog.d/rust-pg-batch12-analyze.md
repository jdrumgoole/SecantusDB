### Rust PostgreSQL server: GROUP BY and DISTINCT order after ANALYZE

Many test suites run ANALYZE after loading their data. PostgreSQL then
plans a GROUP BY or DISTINCT from what ANALYZE measured rather than from
its defaults, and that changes the order the groups come out in. The Rust
PostgreSQL server now keeps what ANALYZE stores and plans from it, measured
against PostgreSQL 15.19.

#### Fixed

- **A GROUP BY or DISTINCT of bare columns over an ANALYZEd table returns
  its groups in PostgreSQL's order.** ANALYZE now records each column's
  `pg_statistic` row as PostgreSQL computes it from a sample of the whole
  table (`stanullfrac`, `stadistinct`, `stawidth`) and `pg_class`'s
  `relpages` and `reltuples`, as a snapshot that rows inserted later do not
  change. The group estimate that sizes the HashAggregate's table is then
  `get_variable_numdistinct` over those statistics. A table analysed
  earlier had kept the server's old order. 212 of 212 shapes (GROUP BY,
  DISTINCT, aggregates, ORDER BY, and a LIMIT where no index leads with a
  group key) match 15.19 at 20 to 10,000 rows, and so do the same shapes after half as many rows
  again, and four times as many, are inserted after the ANALYZE. Part of
  RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_group_by_over_an_analyzed_table_comes_out_in_hash_order).
- **The HashAggregate's table grows when PostgreSQL's does.** simplehash's
  grow threshold is an integer (`size * 0.9`, truncated), and every row's
  lookup checks it, not only a new group's insert. So a table that the
  last group fills grows if more rows follow. Both now hold, which matters
  once the estimate is close to the real group count. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_rows_after_the_last_group_grow_the_hash_table).

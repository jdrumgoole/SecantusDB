### Rust PG server: EXPLAIN proves a DISTINCT or GROUP BY subquery's join side unique

A join whose inner side is a FROM-subquery now prints `Inner Unique` as
PostgreSQL 15.19 decides it (`rel_is_distinct_for` /
`query_is_distinct_for`): a `DISTINCT`, `DISTINCT ON`, `GROUP BY`, plain
aggregate or `UNION` / `INTERSECT` / `EXCEPT` without `ALL` is unique when the
join's equalities fix every key. Before, every subquery printed `false`.

#### Fixed

- `explain (format json) select * from r join (select distinct x from r) s
  on r.x = s.x` prints `"Inner Unique": true`, as PostgreSQL does.
  Fixes RP-356 (tests/backlog/test_rp_explain_join_json.py::test_a_distinct_subquery_inner_side_is_unique).
  The item was filed as RP-355, an ID it shared with the stacked-windows
  item, and is renumbered.

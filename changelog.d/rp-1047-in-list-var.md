### Rust PG server: an IN list holding a column prints as PostgreSQL's parser builds it

`pg_get_viewdef`, `pg_get_constraintdef` and EXPLAIN now print `x IN (...)`
as `transformAExprIn` builds it. Members holding no column of the query
become one `= ANY (ARRAY[...])`, and only when there are two or more of
them. Every other member is a comparison of its own, ORed after it two at a
time (ANDed for NOT IN). A column of an enclosing query counts as a
constant, as in `contain_vars_of_level`. EXPLAIN flattens the chain into
the OR around it, and a NOT IN's into the scan's conjuncts, as the planner
does.

#### Fixed

- `create view v as select c.relnatts in (1, c.relam::int) from pg_class c`
  prints `((c.relnatts = 1) OR (c.relnatts = (c.relam)::integer))`, as on
  PostgreSQL 15.19, not `(c.relnatts = ANY (ARRAY[1,
  (c.relam)::integer]))`.
- `explain select 1 from t where a not in (1, b) and c > 2` filters on
  `((a <> 1) AND (a <> b) AND (c > 2))`.
- A row IN list, `(a, b) in ((1, 2), (3, 4))`, prints as ruleutils does,
  an OR of the row comparisons, where it was left as written. In pretty
  mode a written row `<>` under an AND keeps its parentheses.

Fixes RP-1047 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_an_in_list_holding_a_column; crates/secantus-pgplan/src/ruleutils.rs::tests::an_in_list_holding_a_column_prints_as_transform_a_expr_in_builds_it, ::a_planned_in_list_holding_a_column_is_flattened)

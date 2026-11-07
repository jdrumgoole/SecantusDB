### Rust PG server: a join's WHERE compared with a NULL value keeps no row

In PostgreSQL, `=`, `<`, `<=`, `>` and `>=` are strict: when either side is
NULL the comparison is NULL, and a WHERE does not keep the row. So `select
* from e where exists (select 1 from s join k on s.a = k.a where k.b =
e.b)` leaves out an outer row whose `b` is NULL on PostgreSQL 15.19. Here
that row was returned whenever a joined `k` row's `b` was NULL as well. The
two-table join path checks its WHERE's `column op value` predicates itself,
and its equality treated two NULLs as equal. The same thing happened with
a NULL bound parameter (`where k.b = $1`) and with a NULL scalar subquery
(`where k.b = (select null::text)`). The backlog item had said the bound
parameter was right, which turned out to be wrong when probed.

#### Fixed

- A predicate in that join path now keeps no row when its value or the
  row's column is NULL, whatever the operator. `NOT boolcol`, which keeps
  NULLs, is unchanged.

Fixes RP-437
(tests/backlog/test_rp_correlated_null.py::test_correlated_exists_over_join_null_matches_nothing,
tests/backlog/test_rp_correlated_null.py::test_join_where_null_value_matches_nothing).

#### Found

RP-438 (a scalar or IN subquery in a JOIN's ON is refused with `0A000
SubLink is not supported yet`, where PostgreSQL 15.19 runs it), with its
gap test.

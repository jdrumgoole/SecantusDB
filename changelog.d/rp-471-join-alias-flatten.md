### EXPLAIN of an aliased JOIN plans it as the join it names

A join alias's columns are its sides' own columns, so PostgreSQL plans
`select j.c from (fs join fe on fs.a = fe.a) as j where j.b = 'x'` exactly
as the unaliased query: a Hash Cond on the join and the WHERE pushed down
to `fe`'s scan. The Rust PostgreSQL server's EXPLAIN now does the same,
where before it printed no Hash Cond and kept the WHERE above the join.

#### Fixed

- EXPLAIN flattens an aliased inner JOIN's column references to the
  sides' own (`join_alias::flatten_join_aliases`, PostgreSQL's
  `flatten_join_alias_vars`). A USING / NATURAL / outer join, a `j.*`, a
  subquery or a name the flattening would expose twice is left as written.
  Fixes RP-471 (tests/backlog/test_rp_subquery_scope.py::test_aliased_join_plan)

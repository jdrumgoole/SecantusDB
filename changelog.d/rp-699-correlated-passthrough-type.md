### Rust PG server: a correlated subquery passing an outer column through keeps its type

A correlated subquery that returned an outer FROM function's column unchanged,
or a field of an outer FROM item's whole row, answered it as text: `select
(select f.x) from generate_series(1, 2) f(x)` gave the strings `'1'`, `'2'`
where PostgreSQL gives integers. And `(select (v).*)` was refused.

#### Fixed

- An outer reference to a FROM function's column, or to the function's whole
  row, is typed by the function's result, so the subquery is typed as
  PostgreSQL types the outer Var.
- `(v).a` and `(v).*` inside a subquery, where `v` is an enclosing query's FROM
  item, read that item's column and star (`v.a`, `v.*`), as
  `transformIndirection` does: typed by the column, named after it, and a
  field the item lacks is 42703 `column v.zz does not exist`.

Fixes RP-699 (tests/backlog/test_rp_found_rp517.py::test_correlated_pass_through_keeps_its_type)

Filed while working it: RP-702, RP-703, RP-704, RP-705
(tests/backlog/test_rp_found_rp699.py).

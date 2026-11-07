### A LATERAL item no longer runs its side effects one extra time

On the Rust PostgreSQL server, a `LATERAL` subquery or a function in `FROM`
that reads the row to its left was planned once over sample values to learn
its output's shape, and that planning ran the item's calls. A `nextval` in
the subquery's select list was drawn one more time than the left side has
rows, so `select a, n from fe, lateral (select nextval('lq') + fe.a as n) s`
answered `(1, 3)` where PostgreSQL answers `(1, 2)`. A user function in
such an item ran one extra time as well, so its writes landed twice. The
shape is now planned with no user function called and no sequence drawn,
and each left row runs the item once, as in PostgreSQL 15.19.

#### Fixed

- `secantus-pgplan` (`joins.rs`, `lateral_leaf`): the shape-only plan of a
  lateral item runs under `without_calls`.
- Fixes RP-482 (tests/backlog/test_rp_subquery_scope.py::test_lateral_subquery_draws_once_per_row, tests/backlog/test_rp_subquery_scope.py::test_lateral_item_calls_once_per_row)

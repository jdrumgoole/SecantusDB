### Rust PG server: a `name` compared with a literal stays a `name`

EXPLAIN and view definitions now print `relname = 'pg_class'` as
PostgreSQL 15.19 does, `(relname = 'pg_class'::name)`: `name` has
comparison operators of its own, so the untyped literal takes its type
rather than both sides being cast to `text`.

#### Fixed

- A `name` column compared with an untyped literal (`=`, `<`, `IN`,
  `BETWEEN` ...) was deparsed as `((relname)::text = 'x'::text)`.
  Fixes RP-335 (tests/backlog/test_rp_explain_costs.py::test_plan[a name compared with a literal])

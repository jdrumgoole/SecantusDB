### Rust PG server: `pg_get_partkeydef` prints an expression key as PostgreSQL does

An expression partition key now reads back through `pg_get_partkeydef`
(and psql's `\d+`) as PostgreSQL 15 prints it: through the same ruleutils
printer as `pg_get_expr`, and in a pair of parentheses of its own unless
it looks like a function call.

#### Fixed

- `partition by range ((x + 1))` printed `RANGE ((x + 1))`; it now prints
  `RANGE (((x + 1)))`, a cast `RANGE (((x)::text))`, and a call or a
  call-like form bare: `RANGE (abs(x))`, `RANGE (COALESCE(x, 0))`.
- `pg_get_statisticsobjdef` prints a `NULLIF` expression bare, as
  ruleutils' `looks_like_function` does, rather than in parentheses.

Fixes RP-1052 (tests/backlog/test_rp_found_rp860.py::test_pg_get_partkeydef_of_an_expression_key)

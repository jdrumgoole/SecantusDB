### A FROM function's whole row under a column alias is its value

`select f + 1 from generate_series(1, 2) f(x)` now answers 2, 3 as PostgreSQL
does: the whole row of a function returning a base type is that value,
whatever its column is called, inside an expression (`coalesce(f)`) and in a
subquery (`(select f)`) as well as alone. A field of it, `(select (f).x)`, is
PostgreSQL's 42809 `column notation .x applied to type integer, which is not a
composite type` at the `f`.

#### Fixed

- A FROM function's whole row under a column alias was 42703 `column "f" does
  not exist` inside an expression, and a field of it in a subquery 0A000.
  Fixes RP-702 (tests/backlog/test_rp_found_rp699.py::test_function_whole_row_under_a_column_alias)

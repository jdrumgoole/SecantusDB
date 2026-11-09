### Rust PG server: a WHERE over an array column compares by `array_cmp`

A WHERE that compares an array column with a constant (`=`, `<>`, `<`, `<=`,
`>`, `>=`, BETWEEN, IN, IS DISTINCT FROM) now finds the rows PostgreSQL
finds. It used to be lowered to a MongoDB-style filter. That filter compares
a stored array as a nested document. It also matches an array whose element
equals the operand, sorts a NULL element first, and cannot rank an array
whose lower bound is not 1 at all. The comparison is now evaluated row by
row, the way PostgreSQL's `array_eq` / `array_cmp` do it: the elements in
row-major order, a NULL element after every value, then the counts, the
dimensions and last the lower bounds. The same holds for UPDATE and DELETE.

An untyped literal beside an array is now read as that array's type even when
the array's lower bound is not 1. `select a > '{b}' from t` was 42883 once a
row held such an array.

#### Fixed

- `where a > '{b}'` over `'{cd,ab}'` and `'[0:1]={x,y}'` returns both, as on
  PostgreSQL 15.19 (was only `{cd,ab}`). `>=`, BETWEEN and NOT BETWEEN
  likewise.
- `where a = '{x,y}'` no longer matches `{{x,y},{z,w}}`, and `a <> '{x,y}'`
  and `a NOT IN ('{x,y}')` no longer drop it.
- `where b > '{1}'` over an `int[]` column finds `{NULL,1}` (a NULL element
  sorts last), and `b >= '{1,2}'` finds `'{{1,2},{3,4}}'`.
- `where a is not distinct from null` no longer matches `{x,NULL}`.
- `select k, a > '{b}' from t` answers per row where a row's array has a
  lower bound other than 1 (was 42883 `operator does not exist: text[] >
  text`).

Fixes RP-733 (tests/backlog/test_rp_found_rp722.py::test_a_range_filter_finds_an_array_whose_lower_bound_is_not_1)
Fixes RP-734 (tests/backlog/test_rp_found_rp722.py::test_an_array_column_against_an_untyped_literal_in_the_select_list)

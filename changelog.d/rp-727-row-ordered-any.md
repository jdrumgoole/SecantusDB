### Rust PG server: a row compares with an uncorrelated subquery under an ordering operator

`(k, a) < any (select 2, 3)` and `(a, b, c) >= all (select ...)` now answer
per row, as PostgreSQL does, where they were refused with 0A000. Each
subquery row is compared as a `RowCompareExpr`: decided at the first pair
that is not equal, a NULL at or before it making the comparison NULL.

#### Fixed

- An uncorrelated `row < / <= / > / >= ANY / ALL (subquery)` is answered
  rather than refused, and a zero-length row against a subquery under one
  of those operators is 0A000 `cannot compare rows of zero length` at the
  operator, as on PostgreSQL 15.19.
  Fixes RP-727 (tests/backlog/test_rp_found_rp697.py::test_uncorrelated_ordered_row_any)

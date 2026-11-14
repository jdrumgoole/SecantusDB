### A row's untyped literal takes its pair's type

A string literal inside a row comparison is now typed the way PostgreSQL
types it: each pair of fields resolves its own operator, and the untyped
literal takes the other side's type. `(1, '2') = (1, 2)` is true, `(1, '2')
< any (select 1, 3)` and `(1, '2') in ((1, 2), (3, 4))` answer, and `(1,
'a') < (1, 2)` is 22P02 `invalid input syntax for type integer: "a"` at the
literal, where each used to be refused because the literal was compared as
a string.

#### Fixed

- Rust PG server: an untyped literal field of a row comparison (row against
  row, an IN list of rows, a row against a subquery, and under EXPLAIN) is
  cast to its pair's type at analysis.
  Fixes RP-850 (tests/backlog/test_rp_found_rp727.py::test_untyped_literal_in_a_row_takes_its_pairs_type)

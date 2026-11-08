### Rust PG server: comparing a whole row with a record checks its fields' types

`select 1 from fs where fs = (1,'y')`, where `fs` is `(a int, c text)`, matched
its row, and the same comparison in a select list or in an UPDATE's WHERE
answered true. PostgreSQL compares a composite value with a record through
`record_eq` / `record_cmp`. Those functions check each field pair they reach
before looking at its values, and they refuse two fields of different types.
The bare `'y'` is still `unknown` there, so PostgreSQL 15.19 answers 42804
`cannot compare dissimilar column types text and unknown at record column 2`.
This server compared the values and gave a confident wrong answer.

#### Fixed

- A comparison of a composite value with a record (`=`, `<>`, `<`, `<=`,
  `>`, `>=`, and IN, IS DISTINCT FROM and `= ANY`, which use the same
  operators) now runs PostgreSQL's checks, in its order, on every field
  pair it reaches:
  - two fields of different types are 42804 (`fs = (1,'y'::varchar)`,
    `fs = (null,'y')`);
  - a field that is `unknown` on both sides is 42883 `could not identify an
    equality operator for type unknown` (`a comparison function` for the
    ordering operators);
  - once every shared pair is equal, a record of a different width is 42804
    `cannot compare record types with different numbers of columns`. It
    answered false.

  A pair decided before such a field is never checked, as on PostgreSQL:
  `fs = (2,'y')` is still false. A type modifier is not part of the type.
  Two row constructors are still compared field by field, unchecked.

Fixes RP-522 (tests/backlog/test_rp_subquery_scope.py::test_whole_row_against_untyped_record),
with the wider matrix in `test_rp_subquery_scope.py::test_whole_row_record_checks`
and the check order pinned in the crate by
`tests::composite_record_compare_checks_the_fields_it_reaches`. Files
RP-529 (a domain column's whole row is typed by its base type, so `fd =
row(1)` still answers), RP-530 (a record against a scalar is 0A000, not
42883), RP-531 (NULLIF, GREATEST and LEAST over a whole row) and RP-532
(GROUP BY a whole row not in the select list), found while working it.

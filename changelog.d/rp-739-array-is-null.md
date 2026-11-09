### An array holding a NULL element is no longer NULL in a WHERE

`WHERE b IS NULL` over an array column of the Rust PostgreSQL server found
the rows whose array merely held a NULL element (`'{NULL,1}'`, `'{NULL}'`)
as well as the rows whose array was NULL, and `IS NOT NULL` dropped them --
in a SELECT, an UPDATE and a DELETE alike. PostgreSQL tests only the datum
itself, never its elements. The null test had lowered to the MQL filter
`{b: null}`, which also matches an array holding a null; over an array
column it is evaluated per row now, as the array comparisons have been
since RP-733.

#### Fixed

- `secantus-pgplan`: `IS [NOT] NULL`, `ISNULL`, `NOTNULL` and their
  negations over an array column are evaluated per row instead of as an
  MQL filter.

Fixes RP-739 (tests/backlog/test_rp_found_rp722.py::test_a_null_test_over_an_array_column_reads_the_array,
tests/backlog/test_rp_found_rp722.py::test_a_write_filters_an_array_column_by_its_null_test)

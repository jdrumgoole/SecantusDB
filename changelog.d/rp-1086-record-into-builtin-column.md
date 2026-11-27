### Rust PG server: INSERT ... SELECT refuses a record into a column it cannot coerce to

`insert into t(d) select row(1,'x')` into a `pint[]` column (a domain array),
an `int` or a `jsonb` column was 0A000 `a record cast to ... is not supported
yet`, raised at the row write with no position. Into a `"char"` column, or
from a source that selected no row, it was accepted. PostgreSQL 15.19
refuses each of them with 42804 during parse analysis.

#### Fixed

- A `record` output column of an `INSERT ... SELECT` written to a
  non-composite column is now 42804 `column "d" is of type pint[] but
  expression is of type record`, at the column's position, before a row is
  read. `record` is a pseudo-type, so no `pg_cast` row and no user cast
  starts from it. The only assignment coercion left
  (`find_coercion_pathway`) is the I/O conversion into the string category:
  `text`, `varchar`, `bpchar` and `name` columns still store the record's
  text. Every other target is refused: an `int`, a `"char"`, a `jsonb`, an
  enum, a range, an array, or an array of a user type.
- A `record[]` (`array[row(1,'x')]`) is refused alike, named `record[]`. It
  is stored only in a string-category column or an array of one.

Fixes RP-1086 (tests/backlog/test_rp_found_rp552.py::test_insert_select_record_into_non_composite),
with regressions in the same file
(`test_insert_select_record_into_builtin`).

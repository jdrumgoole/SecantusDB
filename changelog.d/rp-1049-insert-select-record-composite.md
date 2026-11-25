### A `record` column is not written to a composite column by `INSERT ... SELECT`

PostgreSQL converts a row constructor to a composite type field by field
only where the row is written directly, as in `VALUES (row(1, 'x'))`. An
`INSERT ... SELECT` hands the column a plain `record`, and PostgreSQL
refuses `insert into t(c) select row(1, 'x')` with 42846 `cannot cast type
record to ctp` at the row, before it reads a row. The same goes for a
`record` from a subquery in FROM, from a `UNION`, or from `null::record`. The
Rust PostgreSQL server stored the row instead. It now refuses it at the
source column. A column whose type is a domain over a composite is named
as the domain, and `VALUES` still stores the row.

#### Fixed

- `INSERT ... SELECT` of a `record` into a composite column, or into a
  domain over one, is 42846, as on PostgreSQL 15.20.
  Fixes RP-1049 (tests/backlog/test_rp_found_rp552.py::test_insert_select_record_into_composite)

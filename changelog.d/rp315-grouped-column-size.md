### Rust PG server: `pg_column_size` of a grouped column is the stored size, as an int4

`pg_column_size` of a table column under GROUP BY, or as an aggregate's
argument, now reads the stored datum's size and answers `int4`, as
PostgreSQL 15.19 does.

#### Fixed

- `select max(pg_column_size(v)), pg_column_size(j) from t group by 2`
  gave 38 and 28 (the in-memory size, with a 4-byte varlena header) and
  typed them as `text`; it now gives the stored sizes, 35 and 25 (a short
  header), as `int4`. The grouped path turned the column into a parameter
  before the call could see it came from the table; the column's declared
  type now travels with the call, as on the single-table path.
  Fixes RP-315 (tests/backlog/test_rp_found_2026_11_01.py::test_a_grouped_column_size_is_the_stored_one).

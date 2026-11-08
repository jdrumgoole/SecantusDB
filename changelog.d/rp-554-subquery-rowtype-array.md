### Rust PG server: a subquery's whole row, or an array of one, is typed by its row type

A scalar subquery or an `ARRAY(...)` subquery whose column is a row type
(a table's whole row or a `CREATE TYPE ... AS` composite), or an array of
one, now goes out typed by the subquery column's type, as PostgreSQL
15.19's `transformSubLink` types it. Before, its value went out with no
type: it was described as `text` or `text[]`, and psycopg read an array of
rows as `[None]`.

#### Fixed

- `select (select array_agg(fs) from fs)`, the same aggregate over a
  FROM-subquery's whole-row column, `(select array[fs] from fs)` and
  `array(select fs from fs)` are now described as `fs[]` and read back as
  `{"(1,y)"}`. They were `text[]` (1009), which psycopg read as `[None]`.
- `select (select fs from fs)` is now described as `fs` rather than `text`.
- `pg_typeof` of each now names `fs` or `fs[]`; it was 42P18 `could not
  determine data type of parameter $1`. An empty subquery is a NULL of the
  type, or an empty array of it.

Fixes RP-554 (tests/backlog/test_rp_found_rp529.py::test_scalar_subquery_row_type_array, tests/backlog/test_rp_found_rp529.py::test_scalar_subquery_row_type)

#### Found

- RP-571: an `ARRAY(subquery)` of a built-in type is typed from its values
  (`array(select 1::int2)` is `int4[]` here, `smallint[]` on PostgreSQL)
  (tests/backlog/test_rp_found_rp554.py::test_array_sublink_element_type).

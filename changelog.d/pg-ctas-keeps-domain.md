### Rust PG server: `CREATE TABLE AS` keeps a domain column's domain

A table made by `CREATE TABLE AS`, `SELECT ... INTO` or a materialized
view now has a column of a domain wherever its query's column is of one,
as PostgreSQL's does. Every column of a domain also reports the
`atttypmod` PostgreSQL gives it.

#### Fixed

- `intorel_startup` copies the query's tuple descriptor, so `create table
  c as select a from t`, where `a` is of a domain `d` over `char(3)`,
  makes `a` of type `d` on PostgreSQL 15.19. Here the column was made of
  the base type, `character`, because its type was read back from the
  wire description, which sees through the domain. The domain of each
  column is now worked out from the query as written. That covers a
  table's column, one through a WITH or a FROM subquery, `WITH NO DATA`
  and `SELECT ... INTO`. The new column follows the domain's DEFAULT and
  its constraints from then on.
  Fixes RP-661 (tests/backlog/test_rp_found_rp557.py::test_create_table_as_keeps_a_domain_column)
- The copy does not check the domain's constraints again, since
  `intorel_receive` coerces nothing. A row stored before a `NOT VALID`
  CHECK was added is still copied, as `INSERT ... SELECT` copies it
  (tests/backlog/test_rp_found_rp557.py::test_create_table_as_does_not_recheck_a_domain_value).
- `pg_attribute.atttypmod` of a column of a domain is -1, because a
  domain takes no modifier. Here a column of a domain over `char(3)`
  answered the domain's `typtypmod`, 7, in its table and in an index over
  it. `information_schema.columns` still reads the domain's width.
  Fixes RP-558 (tests/backlog/test_rp_found_rp551.py::test_domain_column_atttypmod)
- Filed while working it, already wrong before this change: RP-769. A
  domain over a collatable type has no collation here, so `CREATE VIEW`
  or `CREATE MATERIALIZED VIEW` over its column is refused with 42P16.

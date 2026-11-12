### A catalog read for one relation builds only that relation's rows

After any DDL statement, the Rust PostgreSQL server rebuilt and filled every
user relation's rows in `pg_attribute`, `pg_type` and `pg_class` the next time
a query read them, even when the query asked about one relation. psql's `\d`
asks for exactly that: one table's columns (`attrelid = 't'::regclass`), then
one type and one collation per column. In a debug build its collation query
took 0.11 s straight after a `CREATE TABLE` with 200 other tables, and 0.008 s
after that. PostgreSQL answers in under a millisecond either way, because it
reads one relation's rows through the catalog's oid index.

A read whose filter pins one relation (`pg_attribute.attrelid`, or `oid` in
`pg_class` and `pg_type`) now builds and fills that relation's rows alone. The
rows are the ones the full listing holds for that key, column for column. If a
query asks for a second relation in the same message cycle, as a correlated
subquery does for each outer row, the server builds every row once instead.
The walk that counts catalog reads now counts a sequential scan's rows without
filling them. Filling no longer reads every relation's owner or every index's
access method unless a row needs one, and readers share the decoded table
definitions instead of copying them. The same query now takes about 0.04 s
after DDL with 200 tables. It still grows with the schema (0.049 s with 400
tables, 0.092 s with 800, against 0.010 s with none), so RP-715 stays open for
the remainder.

#### Fixed

- `secantus-pg`: `virtual_rows` builds the rows of the one relation a filter
  pins (`catalog_key`, `catalog_rows_keyed`). The `pg_attribute` arm skips
  every other relation, and nothing fills, qualifies or merges a row the key
  leaves out. A second pinned relation in one cycle falls back to one full
  build.
- `secantus-pg`: the catalog-read walk's index-scan count builds only the
  scanned relation's rows. Its sequential-scan count builds the rows without
  filling them (`catalog_row_count`).
- `secantus-pg`: `fill_catalog_columns` lists the relations' owners
  (`pg_type`) and the indexes' access methods (`pg_class`) only when a row
  needs them, and returns at once for no rows. `shared_table_defs` hands out
  the committed table definitions without copying each one.
- Fixes RP-715 in part
  (tests/test_rust_pgserver_slice.py::test_a_read_pinned_to_one_relation_answers_as_the_full_listing,
  which pins each pinned read to the full listing's rows for its key, in the
  committed catalog and inside a transaction with uncommitted DDL). The gap
  test
  `tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_does_not_grow_with_the_schema`
  now compares the first query after DDL with 400 more tables against the
  same query with none, and keeps RP-715 open.
- Filed RP-909: an index scan of `pg_attribute` with a filter beside the
  equality is counted as a sequential scan.

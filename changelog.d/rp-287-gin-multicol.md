### Rust PG server: a multi-column GIN index has its pages

A GIN index over several columns is now modelled page by page, as
PostgreSQL builds it: each key tuple carries its column's `int2` attnum,
keys sort by column first, and each column's keys of a row enter the
build accumulator as their own run. `pg_relation_size` reads PostgreSQL
15.19's size for it where it read 0, through CREATE INDEX, the pending
list and `fastupdate = off` inserts. Its scans still take a relation
SIREAD lock.

#### Fixed

- `pg_relation_size` of a multi-column GIN index (array, `jsonb` and
  `tsvector` columns, mixed) read 0.

Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_multi_column_gin_index_has_its_pages,
tests/backlog/test_rp_top10_found.py::test_multi_column_gin_index_shapes_have_their_pages).

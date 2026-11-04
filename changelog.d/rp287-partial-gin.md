### Rust PG server: a partial GIN index has its pages (RP-287, part)

A partial GIN index is now modelled page by page like any other GIN index,
holding entries only for the rows its WHERE is true for, at CREATE INDEX
and on every later insert, as `ExecInsertIndexTuples` does.

#### Fixed

- `pg_relation_size` of a partial GIN index read 0; it now reads PostgreSQL
  15.19's size, whether rows were there at CREATE INDEX or came after it,
  with `fastupdate` on or off.
  Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_partial_gin_index_has_its_pages,
  tests/backlog/test_rp_top10_found.py::test_a_partial_gin_index_takes_only_its_rows).

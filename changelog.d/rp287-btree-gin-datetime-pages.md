### A btree_gin index over a date or timestamp column has its pages

A GIN index built with `btree_gin` over a `date`, `timestamp` or
`timestamptz` column is now modelled page by page, so `pg_relation_size`
and the other size functions report the size PostgreSQL 15.19 gives it
instead of 0.

#### Fixed

- `pg_relation_size` of a `btree_gin` index over `date`, `timestamp` or
  `timestamptz` read 0. Fixes RP-287, in part
  (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_date_index_has_its_pages,
  tests/backlog/test_rp_top10_found.py::test_a_btree_gin_timestamp_index_has_its_pages).

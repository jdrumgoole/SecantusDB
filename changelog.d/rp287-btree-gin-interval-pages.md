### A btree_gin index over interval has its pages

A GIN index built with `btree_gin` over an `interval` column now has the
pages PostgreSQL gives it, so `pg_relation_size` and the other size
functions read its real size instead of 0. Its keys are ordered as
`interval_cmp_internal` orders them, with 30-day months and 24-hour days.

#### Fixed

- `pg_relation_size` of a `btree_gin` index over `interval` read 0.
  Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_interval_index_has_its_pages)

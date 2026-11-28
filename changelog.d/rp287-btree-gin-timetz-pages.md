### A btree_gin index over timetz has its pages

A GIN index built with `btree_gin` over a `timetz` column, or a GIN index
over `timetz[]`, now has the pages PostgreSQL gives it, so
`pg_relation_size` and the other size functions read its real size
instead of 0. Its keys are ordered as `timetz_cmp_internal` orders them:
by the time in UTC, then by the zone, so the same instant written in two
zones makes two keys.

#### Fixed

- `pg_relation_size` of a `btree_gin` index over `timetz` (alone, in a
  multi-column index, or as the elements of a `timetz[]`) read 0.
  Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_timetz_index_has_its_pages)

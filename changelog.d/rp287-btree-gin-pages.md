### A btree_gin index has its pages

A GIN index built with `btree_gin` over a plain scalar column -- an
integer, float, boolean or text column -- now reports the size PostgreSQL
gives it. The GIN page model takes the column's value as the row's one key,
as `btree_gin`'s extract functions do, so `pg_relation_size` and the other
size functions no longer read 0 for such an index, whether it was built over
existing rows or filled afterwards through the pending list.

#### Fixed

- `pg_relation_size` of a `btree_gin` index over an `int2`/`int4`/`int8`/
  `float4`/`float8`/`bool`/`text` column matches PostgreSQL 15.19 (it read 0).
  Fixes RP-287, in part (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_index_has_its_pages,
  tests/backlog/test_rp_top10_found.py::test_btree_gin_index_shapes_have_their_pages).

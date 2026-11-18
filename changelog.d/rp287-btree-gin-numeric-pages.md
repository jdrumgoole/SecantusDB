### A btree_gin index over numeric has its pages

A `btree_gin` index over a `numeric` column is now modelled page by page,
each key a varlena `numeric` sized by its base-10000 digits and ordered as
`cmp_numerics` orders them, NaN above every number. `pg_relation_size`
reads the pages PostgreSQL 15.19 builds, where it read 0, and SERIALIZABLE
takes page locks on the index rather than a relation lock.

#### Fixed

- Rust PG server: `pg_relation_size` of a `btree_gin` index over `numeric`
  was 0. Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_btree_gin_numeric_index_has_its_pages,
  tests/backlog/test_rp_top10_found.py::test_a_btree_gin_numeric_index_of_wide_values_and_nan_has_its_pages)

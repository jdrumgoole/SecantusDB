### Rust PG server: `pg_relation_size` of a GIN or GiST index reads its pages

The size functions asked only the B-tree model for an index's pages, so a
GIN or GiST index that the page models already lay out read 0 from
`pg_relation_size`, and `pg_indexes_size` priced it as a heap estimate.

#### Fixed

- `pg_relation_size`, `pg_indexes_size` and `pg_total_relation_size` read a
  GIN (`array_ops`) or GiST (`point_ops`, `box_ops`) index's pages from its
  model, as PostgreSQL 15.19 counts them (4 and 18 pages in the gap test).
  Fixes part of RP-287 (tests/backlog/test_rp_top10_found.py::test_gin_and_gist_indexes_have_their_pages,
  tests/backlog/test_rp_top10_found.py::test_a_tables_gin_and_gist_indexes_size_as_their_pages);
  the item stays open for the indexes not yet modelled.

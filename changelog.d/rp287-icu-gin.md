### A GIN index over ICU-collated text has its pages

A GIN index over a text column (or text array) declared with an ICU
collation, such as `"en-US-x-icu"`, now orders its keys as PostgreSQL's
`varstr_cmp` does under that collation, so the page model builds it and
`pg_relation_size` reports the same size PostgreSQL 15.19 does, where it
read 0 before.

#### Fixed

- Rust PG server: a `btree_gin` or `array_ops` GIN index over ICU-collated
  text is modelled page by page, its keys in the collation's order.
  Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_linguistically_collated_btree_gin_index_has_its_pages)

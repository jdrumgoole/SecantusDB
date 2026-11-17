### A GIN index over a "C"-collated column has its pages

A GIN index over a text column (an array or a `btree_gin` scalar) whose own
collation is `C`, `POSIX` or `C.UTF-8` orders its keys by bytes, exactly as
the page model does, but it was left unmodelled, so `pg_relation_size` read
0 and SERIALIZABLE fell back to a relation lock. It now has PostgreSQL
15.19's pages.

#### Fixed

- `pg_relation_size` of a GIN index over a `C`-collated text column reads
  its pages instead of 0.

Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_collated_btree_gin_index_has_its_pages)

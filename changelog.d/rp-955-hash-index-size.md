### A hash index's size is its pages

`pg_relation_size` of a hash index answered 0, and `pg_indexes_size` and
`pg_total_relation_size` counted a hash index at an estimate: the size
functions read only the B-tree, GIN and GiST page models. They now read the
hash index's metapage model, its buckets with the metapage and bitmap page,
as PostgreSQL's `RelationGetNumberOfBlocks` does. A hash index whose key's
hash is not reproduced keeps its metapage too, since its tuple count alone
decides its splits.

#### Fixed

- Rust PG server: `pg_relation_size`, `pg_indexes_size` and
  `pg_total_relation_size` count a hash index's pages.
  Fixes RP-955 (tests/backlog/test_rp_found_rp929.py::test_a_hash_index_has_its_pages)

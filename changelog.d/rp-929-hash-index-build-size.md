### A hash index's build records its size

A hash index on the Rust PostgreSQL server never had a size in
`pg_class`: every one showed 0 pages and -1 tuples, the answer for an
index that has never been built, however many rows it was built over.
Now each build of a hash index (CREATE INDEX, REINDEX, a table rewrite)
records what PostgreSQL's `hashbuild` leaves: the metapage, the bitmap
page and the buckets `_hash_init` sizes from the table's estimated rows,
and the index tuples it made, NULL keys and rows outside a partial
index's WHERE left out. TRUNCATE forgets the size again, as PostgreSQL
does.

#### Fixed

- `relpages` and `reltuples` of a hash index after CREATE INDEX, REINDEX
  and a rewrite: 6 pages and 0 tuples on an empty `(a int, b text)`
  table, 514 pages over 100,000 rows, as PostgreSQL 15.19. This covers the
  hash indexes SERIALIZABLE does not model (a numeric or collated key, a
  partial or an expression index) as well as those it does.
- A modelled hash index's metapage counted rows with a NULL key as
  inserted, so it could split a bucket PostgreSQL would not.
- A hash index's page count after a bucket split covers the whole
  splitpoint phase `_hash_alloc_buckets` allocates (18 pages for 10
  buckets, not 12), which the SERIALIZABLE planner's cost of a hash index
  scan reads.
- Fixes RP-929 (tests/backlog/test_rp_found_rp921.py::test_a_hash_index_on_an_empty_table_is_recorded_as_built);
  regression tests `tests/backlog/test_rp_found_rp921.py::test_every_hash_index_build_records_its_size`
  and `hash_index::tests::a_split_allocates_its_whole_splitpoint`.
- Filed RP-954 (ANALYZE and VACUUM do not record a hash index's size),
  RP-955 (`pg_relation_size` of a hash index is 0) and RP-956 (no
  overflow pages), with gap tests in `tests/backlog/test_rp_found_rp929.py`.

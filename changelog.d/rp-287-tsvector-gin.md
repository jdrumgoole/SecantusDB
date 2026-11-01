### Rust PG server: a GIN index over a tsvector column has its pages

A GIN index over a `tsvector` column (`tsvector_ops`, the default class) was not
modelled page by page, so `pg_relation_size` read 0 where PostgreSQL 15.19
sizes it in pages: 4 for 2,000 two-word vectors, 163 for 20,000 three-word
ones.

#### Fixed

- `tsvector_ops` joins `array_ops` and `jsonb_ops` in the GIN page model: its
  keys are `gin_extract_tsvector`'s, each lexeme a `text`, ordered as
  `gin_cmp_tslexeme` orders them; a NULL vector is a NullItem and an empty
  one an EmptyItem. The build, the pending list and VACUUM work on it as on
  the other two, and SERIALIZABLE inserts take its page locks. A `@@` scan is
  not modelled yet and keeps the relation lock.
- Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_tsvector_gin_index_has_its_pages).
  What is left (`jsonb_path_ops` and btree_gin indexes, the `jsonb_ops` and
  `tsvector_ops` scans, and the rest of the list) stays on RP-287, now
  verified by `test_a_jsonb_path_ops_gin_index_has_its_pages`.

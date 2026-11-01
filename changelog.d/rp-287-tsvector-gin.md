### Rust PG server: a GIN index over a tsvector column has its pages

A GIN index over a `tsvector` column (`tsvector_ops`, the default class) was not
modelled page by page, so `pg_relation_size` read 0 where PostgreSQL 15.19
sizes it in pages: 4 for 2,000 two-word vectors, 163 for 20,000 three-word
ones. Rows of a `tsvector` column also landed on other heap pages than
PostgreSQL's, so their `ctid`s differed.

#### Fixed

- `tsvector_ops` joins `array_ops` and `jsonb_ops` in the GIN page model: its
  keys are `gin_extract_tsvector`'s, each lexeme a `text`, ordered as
  `gin_cmp_tslexeme` orders them; a NULL vector is a NullItem and an empty
  one an EmptyItem. The build, the pending list and VACUUM work on it as on
  the other two, and SERIALIZABLE inserts take its page locks. A `@@` scan is
  not modelled yet and keeps the relation lock.
- A `tsvector` datum is sized as PostgreSQL lays it out (`tsvector.h`: the
  size word, a 4-byte `WordEntry` per lexeme, each lexeme's bytes, and a
  SHORTALIGNed position count and positions), both for `pg_column_size` and
  for the heap's tuple widths. A `tsvector` column was sized as its text,
  so its rows packed more to a page than PostgreSQL's: 149 to the first
  page where PostgreSQL puts 120, and every `ctid` after it differed.
- Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_tsvector_gin_index_has_its_pages).
  What is left (`jsonb_path_ops` and btree_gin indexes, the `jsonb_ops` and
  `tsvector_ops` scans, and the rest of the list) stays on RP-287, now
  verified by `test_a_jsonb_path_ops_gin_index_has_its_pages`.
- Filed RP-315: `pg_column_size` of a column under GROUP BY reads the
  in-memory size, not the stored one (tests/backlog/test_rp_found_2026_11_01.py).

### Rust PG server: a `jsonb` GIN index and a `jsonb` table have PostgreSQL's pages

A GIN index over a `jsonb` column (`jsonb_ops`, the default opclass) is now
modelled page by page, as `array_ops` indexes already were. Before, its
`pg_relation_size` read 0 where PostgreSQL 15.19 reads 4 pages. Its keys
are `gin_extract_jsonb`'s: object keys, values flagged by type, string
array elements keyed as keys, numbers by their normalised text, and strings
over 125 bytes by their `hash_any`. CREATE INDEX, the pending list and
inserts with `fastupdate` off all build the same pages PostgreSQL builds.

The index model needed the heap's TIDs, and they were wrong too. A `jsonb`
datum was sized as its text, so 186 small rows fitted a heap page where
PostgreSQL fits 136. A `jsonb` value is now sized as `convertToJsonb`
builds it, so a `jsonb` table's `pg_relation_size`, its `ctid`s and the
plans priced from its pages match PostgreSQL's.

#### Fixed

- `pg_relation_size` of a `jsonb_ops` GIN index, and of a table with a
  `jsonb` column; a `jsonb` row's `ctid`.
  Fixes RP-287 in part (tests/backlog/test_rp_top10_found.py::test_a_jsonb_gin_index_has_its_pages,
  tests/backlog/test_rp_top10_found.py::test_jsonb_ops_indexes_and_their_heaps_have_postgres_pages).
  `jsonb_ops` scans, `tsvector_ops` and the other opclasses are still open
  under RP-287.

#### Found

- RP-310: a UNION branch selecting a bare NULL from a relation is typed
  `text`, so `select 1 union all select null from (values (1)) v` raises
  42804 where PostgreSQL answers the rows.

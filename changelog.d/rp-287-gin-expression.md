### Rust PG server: an expression GIN index has its pages

A GIN index over an expression -- `gin ((a || array[0]))`,
`gin (to_tsvector('simple', s))`, `gin ((j -> 'k') jsonb_path_ops)`, or an
expression beside a plain column -- is now modelled page by page, each
row's keys extracted from the expression's value as PostgreSQL's
`FormIndexDatum` computes it, and typed by the expression's result type.
`pg_relation_size` reads PostgreSQL 15.19's size for it where it read 0,
through CREATE INDEX, the pending list, `fastupdate = off` inserts and a
partial index's predicate. Its scans still take a relation SIREAD lock.

#### Fixed

- `pg_relation_size` of an expression GIN index (array, `text[]`,
  `tsvector` and `jsonb` expressions, partial, or beside a column) read 0.

Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_an_expression_gin_index_has_its_pages,
tests/backlog/test_rp_top10_found.py::test_expression_gin_index_shapes_have_their_pages).

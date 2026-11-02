### Rust PG server: a `jsonb_path_ops` GIN index has PostgreSQL's pages

A GIN index built with `jsonb_path_ops` is now modelled page by page, so
`pg_relation_size` of one reads the pages PostgreSQL 15.19 builds instead
of 0.

#### Fixed

- Each `jsonb` value's keys are `gin_extract_jsonb_path`'s: every leaf
  keyed by the int4 hash of the object keys on its path and its own value,
  folded as `JsonbHashScalarValue` folds them (`hashtext`, `hash_numeric`,
  the null and boolean constants), sorted as `int4` sorts them. RP-287
  stays open for its other parts.

Fixes RP-287, part (tests/backlog/test_rp_top10_found.py::test_a_jsonb_path_ops_gin_index_has_its_pages)

### Rust PG server: PostgreSQL's own relations list their system columns, collations and array dimensions

`pg_attribute` over the built-in relations now matches PostgreSQL 15.19 row
for row: the system columns (`ctid`, `xmin`, `cmin`, `xmax`, `cmax`,
`tableoid`) of every catalog with storage are listed, and each column's
`attcollation`, `attndims` and `attstorage` are the ones PostgreSQL records
rather than ones inferred from the type.

#### Fixed

- The 648 system-column rows (`attnum < 0`) of the built-in relations were
  missing from `pg_attribute`.
- A catalog's collatable column reported its type's default collation where
  PostgreSQL records `C` (950); an array column reported `attndims` 0 where
  PostgreSQL records 1; and a TOAST table's `chunk_data` reported extended
  storage where PostgreSQL records plain.

Fixes RP-302 (tests/backlog/test_rp_catalog_identity.py::test_builtin_relations_attributes)

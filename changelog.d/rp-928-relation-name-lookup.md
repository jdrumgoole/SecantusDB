### A new table's name is checked by lookup, in its own schema

`CREATE TABLE` on the Rust PostgreSQL server used to check that its name
was free by listing every composite type and every table's indexes, so
the check got slower as the schema grew. It now finds a clashing
composite by its catalog key and a clashing index through a name index
in storage, which is kept current from the index catalog's change log,
as PostgreSQL finds one through `pg_class`'s name index. The index check
now also looks only in the new table's schema, as PostgreSQL does. Before
this, an index in one schema blocked a table of the same name in
another, and a composite type matched by its bare name in any schema.

#### Fixed

- `CREATE TABLE`'s relation-name checks no longer grow with the number of
  tables (`Storage::index_owners`, `composite_keyed`). The storage change
  is in `secantus-storage`, which the MongoDB server also links.
- `CREATE TABLE s.x` is refused when schema `s` has an index `x`, and
  `CREATE TABLE x` in `public` is not, as on PostgreSQL 15.19.

Trims RP-928 (tests/backlog/test_rp_catalog_latency.py::test_create_table_costs_the_same_at_800_tables_within_a_tenth); regression test tests/test_rust_pgserver_relation_names.py::test_create_table_finds_a_clashing_index_or_composite_by_name

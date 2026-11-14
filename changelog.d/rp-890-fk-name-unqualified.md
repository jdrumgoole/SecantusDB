### A generated foreign-key name no longer carries the table's schema

A foreign key declared without a name on a table outside `public`, or on a
temporary table, was named from the schema-qualified table key, so it was
stored as `s.child_b_fkey` rather than PostgreSQL's `child_b_fkey`. A DROP's
dependency DETAIL showed the qualified name, and `ALTER TABLE ... DROP
CONSTRAINT child_b_fkey` could not find the constraint. The generated name is
now built from the bare relation name, as PostgreSQL does.

#### Fixed

- Rust PG server: an unnamed foreign key on a schema-qualified or temporary
  table is named `<table>_<cols>_fkey`, without the schema.
  Fixes RP-890 (tests/backlog/test_rp_found_rp565.py::test_a_foreign_key_in_a_drop_detail_is_named_by_its_own_name)

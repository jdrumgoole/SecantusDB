### An index outside `public` can be renamed

`ALTER INDEX s.ix RENAME TO ix2`, and the bare name with `s` on the search
path, now rename the index of a table in schema `s`, as PostgreSQL does;
the Rust PostgreSQL server matched the schema-qualified target against the
bare index name and answered 42P01.

#### Fixed

- Rust PG server: `ALTER INDEX ... RENAME` resolves a schema-qualified index.
  Fixes RP-933 (tests/backlog/test_rp_found_rp923.py::test_an_index_outside_public_can_be_renamed)

### information_schema.columns names a column's collation, declared or not

`information_schema.columns.collation_name` used to show only a collation
a column declared with `COLLATE`, so a `name` column, a `name[]` column
and a column of a domain over `name` showed NULL, and a declared
`collate "default"` showed `default`. PostgreSQL builds the view from the
column's `attcollation` -- its own COLLATE, else its type's -- and leaves
out only `pg_catalog.default`; the Rust PostgreSQL server now does the
same, so those columns show `C` and the declared default shows nothing.

#### Fixed

- `information_schema.columns`' collation columns follow `attcollation`.
  Fixes RP-783 (tests/backlog/test_rp_found_rp769.py::test_information_schema_columns_collation_is_the_columns)

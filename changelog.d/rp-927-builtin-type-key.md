### psql's `\d` after DDL reads each built-in column type alone

The column query psql runs for `\d` joins `pg_attribute` to `pg_type` per
column. On the Rust PostgreSQL server, the first run straight after DDL
built `pg_type` keyed to the first column's type. The second column's
type, a second key in the same message cycle, built every row: every
table's row type, each with its owner. So the query grew with the
schema: a debug build took 0.016 s with no other table and 0.10 s with
800, where PostgreSQL 15.19 reads one row per column through the oid
index. A key of a built-in type builds only the built-in rows and reads
nothing a user made, so its cost does not depend on the schema. It is now
always built alone and no longer counts as the cycle's other key.

#### Fixed

- A `pg_type` read keyed to a built-in oid is built alone however many
  keys the message cycle has already built (`virtual_rows`).
- A key of a user type still builds every user type, filed as RP-970.

Fixes RP-927 (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_over_two_column_types_costs_the_same_at_400_tables)

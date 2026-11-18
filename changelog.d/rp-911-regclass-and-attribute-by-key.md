### The first catalog query after DDL no longer grows with the schema

A `'t'::regclass` now finds its one relation by name, as PostgreSQL's
`get_relname_relid` reads `pg_class`. It used to list every table, index,
view and sequence in the database first. A `pg_attribute` read keyed to one
relation's oid now finds that relation, its row type and its parents through
an index by oid. That index is kept current from the catalog documents each
commit changed. Before, the read mapped every relation, scanned every
composite and filled its rows from a map of every table. psql's `\d`
collation query, the first time it runs after a `CREATE TABLE`, used to
take a fifth to a quarter longer with 800 other tables than with none
(debug build, arm64 macOS). It now costs within a tenth of the same.

#### Fixed

- Fixes RP-911 (tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_800_tables_within_a_tenth), which now runs on every platform, not only macOS.

### The first catalog query after DDL no longer reads every table's name

A client that describes a table straight after creating it (`psql`'s `\d`,
an ORM reflecting its schema) runs a `pg_attribute` read keyed by the
table's oid. The Rust PostgreSQL server found that table's few names and
then still walked every table's definition comparing names, so on a schema
of thousands of tables the read cost grew with the schema: about 0.7 ms at
3,200 tables on a Linux x86_64 CI runner, where PostgreSQL reads the rows
through its `attrelid` index. It now looks each name up through the catalog
version's index of names. The parse memo also kept adding up the length of
every statement text it held on each new one; it now keeps that total as
entries come and go.

#### Fixed

- `secantus-pg`: a keyed `pg_attribute` read looks up only the definitions
  of the names its oid can be (`table_def_position`), in the definitions'
  order (RP-1100, `tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_the_same_at_3200_tables_within_a_tenth`,
  whose ratio on CI went from 1.13-1.16 to 1.06-1.07).
- `secantus-pgplan`: `parse_tree`'s memo holds the bytes of text it keeps
  beside the trees, rather than summing every key on each parse.

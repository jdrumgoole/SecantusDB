### The statement after DDL no longer reads every table's index list

After any DDL, the Rust PostgreSQL server rebuilds the catalog state its
planner reads. On a schema of 800 tables that work read every table's index
list and built the list `regclass` resolves names against, even for a
statement that named no relation. A table's index list is now read only
when the storage's index-name map, which is kept current from the index
catalog's change log, says the table holds an index. The relation list is
built when a statement first resolves a relation, much as PostgreSQL reads
`pg_class` only for a name it is asked about. In a debug build, a
`SELECT 1` straight after a `CREATE TABLE` at 800 tables went from 8.1 ms
to 4.0 ms (median).

#### Changed

- `secantusd-pg`: the index relations after DDL skip tables that hold no
  index (`Storage::indexed_collections`), and `regclass`'s relation list
  is derived at a statement's first lookup. Part of RP-911 and RP-928;
  both stay open. Regression test:
  `tests/test_rust_pgserver_relation_names.py::test_a_tables_index_relations_follow_its_index_ddl`.
  The change is in the shared `secantus-storage` crate, which the MongoDB
  server also links (a new read-only method; no existing path changed).

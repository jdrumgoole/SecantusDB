### A statistics read on a new connection counts its own catalog lookups

PostgreSQL parses and plans a statement before it runs it, and the scan
counters in `pg_stat_xact_sys_tables` and its relatives are read live, so a
query of those views already includes the catalog lookups made to plan it.
The Rust PostgreSQL server counted a statement's catalog reads after it had
answered. On a fresh connection, the first read of a statistics view left
its own lookups to show up in the next read: an index scan of
`pg_attribute` measured between two reads came out as two, alongside
`pg_amop`, `pg_class`, `pg_operator` and `pg_type` scans PostgreSQL never
reports there. A statement that reads the counters is now counted before it
runs, so the change between two reads matches PostgreSQL 15.19 from a
connection's first statement onward.

#### Fixed

- `secantus-pg`: a statement naming a `pg_stat*` view or function has its
  catalog reads walked before it executes (`catalog_reads::reads_counters`),
  except in a failed transaction block, which analyses nothing.
- Fixes RP-909 (tests/backlog/test_rp_catalog_reads.py::test_an_index_scan_with_a_filter_counts_one_index_scan);
  `tests/test_rust_pgserver_slice.py::test_an_equality_with_other_quals_is_one_index_scan`
  now measures from a fresh backend.

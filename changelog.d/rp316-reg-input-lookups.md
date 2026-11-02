### Rust PG server: a reg* literal looks its name up in the catalogs

A cast of a string to `regclass`, `regtype`, `regproc` or `regnamespace` now
counts the catalog lookups PostgreSQL 15.19's input functions make for it
in `pg_stat_sys_indexes`. Before, the cast counted none, and a second
literal of the same statement shape reused the first one's counts.

#### Fixed

- `'pg_class'::regclass` scans `pg_class_relname_nsp_index` along the
  search path (an explicit schema is looked up by name first);
  `'int4'::regtype` parses its string as a type name and looks it up;
  `'now'::regproc` (also through `::text`) makes `regprocin`'s list search
  of `pg_proc_proname_args_nsp_index`; `'pg_catalog'::regnamespace` scans
  `pg_namespace_nspname_index`. An oid string looks nothing up, as in
  PostgreSQL.
- A statement with a reg* literal is no longer counted from the memo of an
  earlier statement with the same shape and a different name.

Fixes part of RP-316 (tests/backlog/test_rp_catalog_reads.py::test_the_reg_input_functions_look_names_up); the rest of RP-316 stays open.

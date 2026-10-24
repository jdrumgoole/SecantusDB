### Rust PostgreSQL server: a system catalog's size, a table's row type oid

A system catalog reports a size, and a table's row type has an oid of its
own.

#### Fixed

- A system catalog has a size (RP-214). `pg_relation_size(1259)` was NULL
  and `pg_relation_size('pg_class')` 0; each catalog's rows are now laid
  out through the heap page model as a fresh `initdb` would lay them out
  (8 KiB pages, a 24-byte page header, a line pointer and MAXALIGNed tuple
  per row), and `pg_indexes_size` counts the catalog's own indexes. The
  bytes are the mechanism's, not PostgreSQL's: its sizes carry each
  cluster's churn (`pg_class` is 114688 on the private 15.19 reference
  cluster, 98304 here). Decided by Joe, 2026-10-24. Fixes RP-214
  (tests/backlog/test_rp_batch52.py::test_scenario[a system catalog's size]).
- A table's row type has an oid of its own (RP-205). The relation and its
  row type were one number, so `'t'::regclass::oid = 't'::regtype::oid`
  and `pg_depend` rows about the type read as rows about the table (`type
  t[]` was listed among what depends on table `t`). CREATE TABLE now draws
  the relation's oid from the counter and then the row type's, as
  `heap_create_with_catalog` does; `pg_type.typrelid` names the relation.
  A table created before keeps its one oid. Fixes RP-205
  (tests/backlog/test_rp_batch52.py::test_scenario[pg_depend of a table and its row type]).

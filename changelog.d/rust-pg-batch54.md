### Rust PostgreSQL server: object identities and addresses, a catalog's size, a row type's oid

`pg_identify_object`, `pg_identify_object_as_address` and
`pg_get_object_address` are served. A system catalog reports a size, and a
table's row type has an oid of its own. Seven divergences found on the way
are fixed with them.

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
- `pg_identify_object`, `pg_identify_object_as_address` and
  `pg_get_object_address` are served (RP-199). An object is identified by
  `(classid, objid, objsubid)` as PostgreSQL 15.19's
  `getObjectIdentityParts` writes it -- `(table,pg_catalog,pg_class,pg_catalog.pg_class)`,
  types as `format_type_be_qualified` names them (`integer`,
  `pg_catalog.text`) -- for every catalog `pg_describe_object` serves, and
  a `(type, names, args)` address resolves back to it, with PostgreSQL's
  error for a missing or mistyped object. Each is a record in a select list,
  a row in FROM and a row of fields under `(f(...)).*`. Fixes RP-199
  (tests/backlog/test_rp_batch51.py::test_scenario[pg_identify_object and its siblings],
  tests/backlog/test_rp_batch54.py::test_scenario[every kind of object identified, and found again from its address],
  tests/backlog/test_rp_batch54.py::test_scenario[pg_identify_object as a value, a row and its fields],
  tests/backlog/test_rp_batch54.py::test_scenario[pg_get_object_address refuses as PostgreSQL does]).
- `CREATE POLICY`, `ALTER POLICY`, `CREATE STATISTICS` and `CREATE` /
  `ALTER PUBLICATION` find a table in another schema, by its qualified name
  or through `search_path`; each read it as `public`'s (42P01). A
  statistics object is created in the schema its name gives, the table's
  without a name, else the creation schema; it was always in `public`.
  Fixes RP-217 (tests/backlog/test_rp_batch54.py::test_scenario[a policy, statistics object and publication on a schema's table],
  tests/backlog/test_rp_batch54.py::test_scenario[a statistics object's schema]).
- A `regprocedure` names an argument type in its schema
  (`textlen(pg_catalog.text)`, as `pg_identify_object_as_address` writes
  it); it was 42883. Fixes RP-218
  (tests/backlog/test_rp_batch54.py::test_scenario[a regprocedure's argument types in their schema]).
- `to_regoper` and `to_regoperator` are served; they were refused. Fixes
  RP-219 (tests/backlog/test_rp_batch54.py::test_scenario[to_regoper and to_regoperator]).
- `x.*` over a function in FROM (`select x.* from json_each(j) x`) lists
  the function's columns; it was refused. Fixes RP-220
  (tests/backlog/test_rp_batch54.py::test_scenario[a FROM function's columns by its alias]).
- A domain constraint's `pg_constraint.connamespace` is its domain's
  schema; it was `public`'s. Fixes RP-221
  (tests/backlog/test_rp_batch54.py::test_scenario[a domain constraint's schema]).
- An unnamed `CREATE STATISTICS`, which PostgreSQL 16 added, is 15.19's
  42601 at its `ON`; it was accepted. `IS JSON`'s syntax error quotes the
  token as the statement spells it (`json`). Fixes RP-222
  (tests/backlog/test_rp_batch54.py::test_scenario[syntax PostgreSQL 16 added is refused as 15 refuses it]).
- `pg_options_to_table` of an untyped array literal
  (`'{fillfactor=70}'`) lists its options; it listed none. Fixes RP-223
  (tests/backlog/test_rp_batch54.py::test_scenario[pg_options_to_table of an untyped array literal]).
- An error raised by an executor-answered function keeps its DETAIL and
  HINT; only the message and CONTEXT reached the client.

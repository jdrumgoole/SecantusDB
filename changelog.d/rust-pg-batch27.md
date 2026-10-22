### Rust PostgreSQL server: catalogs record who owns what, and FROM functions read the row beside them

Schema privileges and the owners of schemas and types are recorded as
PostgreSQL records them; `pg_shdepend` is shared across databases;
`pg_largeobject`, `pg_attribute.attislocal` and the catalogs' expression
columns match PostgreSQL 15.19; a function in FROM reads the row to its left
with or without `WITH ORDINALITY`; and names resolve against the search path
as PostgreSQL's do.

#### Fixed

- Fixes RP-111 (`tests/backlog/test_rp_batch26.py::test_scenario[a type off the search path]`):
  with `public` off the search path (as pg_dump sets it), a bare user type
  name is unknown -- `'mood'::regtype` is 42704 and `to_regtype('mood')`
  NULL -- where it resolved to `public.mood`.
- Fixes RP-106 (`tests/backlog/test_rp_batch26.py::test_scenario[an unknown name in a FROM function]`):
  a bare name in a FROM function's arguments that neither the subquery nor
  the query around it has is `column "a" does not exist` (42703), not
  `ColumnRef is not supported yet`.
- Fixes RP-107 (`tests/backlog/test_rp_batch26.py::test_scenario[expression columns' types]`):
  the catalogs' expression columns (`pg_attrdef.adbin`, `pg_index.indexprs`
  / `indpred`, `pg_class.relpartbound`, `pg_policy.polqual`, `pg_rewrite`'s,
  `pg_trigger.tgqual`, `pg_type.typdefaultbin`, `pg_proc.proargdefaults` /
  `prosqlbody` and others) are `pg_node_tree` (194) and
  `pg_attribute.attmissingval` `anyarray` (2277), where they were `text`.
- Fixes RP-110 (`tests/backlog/test_rp_batch26.py::test_scenario[a large object's pages]`):
  `pg_largeobject` lists each 2,048-byte page a write reached, as long as its
  furthest byte, as PostgreSQL's `inv_write` / `inv_truncate` leave them; a
  write past the end no longer shows the pages between as zeros. A large
  object written before this keeps its contiguous pages.
- Fixes RP-109 (`tests/backlog/test_rp_batch26.py::test_scenario[a merged inherited column]`):
  a column a child declares beside a parent's (`create table c (x int)
  inherits (p)` where `p` has `x`) is merged and stays local
  (`attislocal = true`, `attinhcount = 1`), as `MergeAttributes` leaves it.
- Fixes RP-103 (`tests/backlog/test_rp_batch26.py::test_scenario[schema privileges]`):
  `GRANT` / `REVOKE` on a schema is recorded in `pg_namespace.nspacl`, and
  pg_dump keeps it.
- Fixes RP-104 (`tests/backlog/test_rp_batch26.py::test_scenario[owners of what another role creates]`,
  `tests/backlog/test_rp_batch26.py::test_other_databases_objects_in_pg_shdepend`):
  `CREATE SCHEMA ... AUTHORIZATION r` is owned by `r`, and a type by the
  role that created it, where both reported the bootstrap superuser;
  `pg_shdepend` lists every database's objects, as the shared catalog it is,
  not only the current one's. A role created earlier in an open transaction
  is named by `::regrole` in it.
- Fixes RP-098 (`tests/backlog/test_rp_batch26.py::test_scenario[a record's fields through a subquery]`):
  `(ss.x).n` / `(ss.x).x` over a subquery's `_pg_expandarray(arr)` column
  are typed from the function's OUT parameters (`integer`, the array's
  element), as PostgreSQL's `expandRecordVariable` types them, so
  `information_schema.element_types` answers on a database without user
  functions and `parameters` is typed as PostgreSQL types it.
- Fixes RP-113 (`tests/backlog/test_rp_batch26.py::test_scenario[a FROM function over the row to its left]`):
  `unnest(a) WITH ORDINALITY` beside the table it reads, and
  `_pg_expandarray(a)` over a table's column, failed with `ColumnRef is not
  supported yet`; a function in FROM reads the row to its left whether or
  not it says LATERAL.
- Fixes RP-114 (`tests/backlog/test_rp_batch26.py::test_scenario[an unknown name in a FROM function beside a table]`):
  a name no FROM item has, in a FROM function's arguments beside a table,
  is `column "b" does not exist` (42703), not 0A000.

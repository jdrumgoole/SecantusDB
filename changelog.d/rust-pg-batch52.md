### Rust PostgreSQL server: pg_describe_object, and a block's own definitions

`pg_describe_object` describes any object as PostgreSQL 15.19's
`getObjectDescription` does. Inside a transaction block, `pg_get_indexdef`
and `shobj_description` now answer for what the block itself created or
commented. Six divergences found on the way are fixed: they are listed below.

#### Added

- Most of RP-199
  (`tests/backlog/test_rp_batch51.py::test_scenario[pg_describe_object]`):
  `pg_describe_object(classid, objid, objsubid)` was refused. It now
  describes an object of every catalog, from this server's own catalog rows:
  relations and their columns, functions, types, casts, collations,
  constraints, defaults, operators, operator classes, families and their
  members, rules, triggers, schemas, statistics, the text-search objects,
  roles, databases, extensions, publications, subscriptions, transforms and
  the rest. A name is qualified when the search path does not reach it. A
  missing object is NULL; a class that is not a catalog is XX000. Swept
  against PostgreSQL over every built-in object of 31 catalogs: they agree,
  except for the ICU-only collations (a scope decision) and the system
  views' rules (RP-203, below).

#### Fixed

- Fixes RP-200
  (`tests/backlog/test_rp_batch51.py::test_scenario[pg_get_indexdef of an index the block created]`,
  `tests/backlog/test_rp_batch52.py::test_scenario[a block's own definitions, and no other session's]`):
  the planner's published tables of index definitions and shared comments
  were built before each statement took the block's own transaction, so an
  index the block created had no `pg_get_indexdef` (NULL) and a comment it
  wrote on a database had no `shobj_description`. A block that has written
  the catalog now builds them through its own transaction. Another session
  still sees neither, and after ROLLBACK neither is seen.
- Fixes RP-204
  (`tests/backlog/test_rp_batch52.py::test_scenario[an executor function in a PL/pgSQL expression]`):
  in a PL/pgSQL expression, a function the executor answers (`nextval`,
  `pg_describe_object`, the size functions) was planned without the
  executor's hook, so `x := nextval('nosuch')` assigned NULL where
  PostgreSQL raises 42P01.
- Fixes RP-206
  (`tests/backlog/test_rp_batch52.py::test_scenario[a view over another schema's table depends on it]`):
  a view over a table outside `public` had no `pg_depend` row on the
  table's columns.
- Fixes RP-207
  (`tests/backlog/test_rp_batch52.py::test_scenario[an index in another schema is listed in it]`):
  `pg_class` listed an index in a schema other than `public` under
  `public`.
- Fixes RP-208
  (`tests/backlog/test_rp_batch52.py::test_scenario[a built-in row type's name]`):
  a built-in row type or its array (`pg_type`, `information_schema.tables`)
  printed as its oid through `::regtype` and as `???(71)` through
  `format_type`, and its name did not resolve to it.
- Fixes RP-209
  (`tests/backlog/test_rp_batch52.py::test_scenario[plpgsql's extension oid]`):
  `plpgsql`'s extension oid was 13826. It is 14031, as `initdb` leaves it.
- Fixes RP-210
  (`tests/backlog/test_rp_batch52.py::test_scenario[the visibility functions of an oid that names nothing]`):
  `pg_type_is_visible`, `pg_function_is_visible` and ten more answered
  true for every oid, one that names nothing included (PostgreSQL: NULL),
  and for an object an earlier schema on the path hides. They now look the
  object up: its bare name, with its argument types for a function or an
  operator, must find it.

#### Known

- RP-199, what is left: `pg_identify_object`, `pg_identify_object_as_address`
  and `pg_get_object_address` are refused.
- RP-203: `pg_rewrite` lists no system view's rule.
- RP-205: a table's row type has the table's oid.

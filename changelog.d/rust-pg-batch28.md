### Rust PostgreSQL server: PostgreSQL's built-in functions, types and dependencies in the catalogs

`pg_proc`, `pg_type` and `pg_constraint` list PostgreSQL 15.19's built-in
objects, so `information_schema.routines` and its neighbours answer
PostgreSQL's rows; `pg_depend` records what each object depends on;
`pg_trigger` lists a foreign key's internal triggers. The `information_schema`
views over all the built-ins are correct but slow (RP-131).

#### Fixed

- Fixes RP-116 (`tests/backlog/test_rp_batch26.py::test_scenario[a composite type's relation]`):
  a standalone composite type's relation (`CREATE TYPE t AS (...)`, relkind
  `c`) is named by `::regclass`; reading or writing it as a table is
  `"t" is a composite type` and `DROP TABLE` of it `"t" is not a table`
  (42809), where both said the relation did not exist.
- Fixes RP-118 (`tests/backlog/test_rp_batch26.py::test_scenario[an index created in the open block]`):
  an index created in the open block is a relation to `::regclass` in that
  block.
- Fixes RP-115 (`tests/backlog/test_rp_batch26.py::test_scenario[triggers PostgreSQL makes and their functions]`):
  `pg_trigger` lists the four internal triggers PostgreSQL makes for each
  foreign key (`RI_ConstraintTrigger_a_<oid>` / `_c_<oid>`, `tgisinternal`,
  with the `RI_FKey_*` function for its action, the constraint, its index
  and the other table), and a user trigger's `tgfoid` is its function,
  where it was 0.
- Fixes RP-119 (`tests/backlog/test_rp_batch26.py::test_scenario[regproc names]`):
  `regproc` output quotes a name that needs it (`"RI_FKey_check_ins"`) and
  qualifies a function whose schema is off the search path; input reads a
  quoted part's case (`'pg_catalog."RI_FKey_check_ins"'::regproc`), a
  schema-qualified user function, and a bare name through the search path.
- Fixes RP-120 (`tests/backlog/test_rp_batch26.py::test_scenario[the plpgsql language's oid]`):
  `plpgsql`'s `pg_language.oid` is 14035, as PostgreSQL 15.19's `initdb`
  leaves it, where it was PostgreSQL 14's 14078 (`pg_proc.prolang` and the
  validators with it).
- Fixes RP-121 (`tests/backlog/test_rp_batch26.py::test_scenario[a function's user-typed arguments]`):
  `pg_proc.proargtypes` / `prorettype` / `proallargtypes` name a user type
  (or an array of one) a function takes or returns, where they were 0.
- Fixes RP-122 (`tests/backlog/test_rp_batch26.py::test_scenario[a trigger's UPDATE OF columns]`):
  `pg_trigger.tgattr` holds the columns of `UPDATE OF c, a`, where it was
  empty.
- Fixes RP-123 (`tests/backlog/test_rp_batch26.py::test_scenario[a vector as an array]`):
  an `int2vector` / `oidvector` cast to an array keeps its lower bound of 0
  (`[0:1]={1,2}`), as PostgreSQL's does.
- RP-105 is down to a constant's type and a range's functions
  (`tests/backlog/test_rp_batch26.py::test_scenario[dependencies beyond relations]`
  now pins the rest): `pg_depend` records what PostgreSQL's
  `recordDependencyOn` calls leave for every kind of object -- relations,
  types, arrays and domains, functions (schema, language, argument and
  result types), constraints (key columns `a`, the columns a CHECK reads
  `n`, a foreign key's referenced columns and index), indexes (key,
  expression and predicate columns; one backing a constraint is part of
  it), triggers, rules, views' and materialized views' queries, policies --
  where it held only sequences, defaults, partitions and index keys.
- Fixes RP-124 (`tests/backlog/test_rp_batch26.py::test_scenario[a materialized view's rule]`):
  a materialized view has its `_RETURN` rule in `pg_rewrite`, as a view does.
- Fixes RP-102 (`tests/backlog/test_rp_batch26.py::test_scenario[built-in objects in the catalogs]`):
  `pg_proc` lists PostgreSQL 15.19's 3,244 built-in functions,
  `pg_type` its 611 built-in types (the system relations' row types among
  them) and `pg_constraint` the system catalogs' 111 constraints, as a
  fresh database does, with users' objects after them, so
  `information_schema.routines` / `parameters` / `data_type_privileges`
  answer PostgreSQL's rows. The rows are dumped from 15.19 by
  `tools/probes/pg15_system_catalog_dump.py` into `pg15_catalog_rows.txt`
  (Joe, 2026-10-22); `pg_type` and `pg_constraint` were dumped too rather
  than derived from the system-relation tables, which hold no array types
  or I/O functions.
- Fixes RP-125 (`tests/backlog/test_rp_batch26.py::test_scenario[has_function_privilege by oid]`):
  `has_function_privilege` takes a function's oid (or digits, as
  `regprocedurein` reads them) -- a built-in is PUBLIC's to EXECUTE, an
  unknown oid NULL -- where it said `expected a left parenthesis`; and the
  role `public` is PUBLIC, as `get_role_oid_or_public` reads it.
- Fixes RP-126 (`tests/backlog/test_rp_batch26.py::test_scenario[information_schema's namespace]`):
  `information_schema`'s oid is 13679, where PostgreSQL 15.19's `initdb`
  puts it; 13 stood in.
- Fixes RP-127 (`tests/backlog/test_rp_batch26.py::test_copy_into_a_char_array`):
  `COPY` (and a text-format parameter) into a `"char"[]` column stores an
  array, where it stored the text, which no subscript could read.
- Fixes RP-128 (`tests/backlog/test_rp_batch26.py::test_scenario[a view inside a FROM subquery]`):
  a view read inside a FROM subquery, or inside an `IN (SELECT ...)`,
  expands with the statement, so the scalar subqueries an
  `information_schema` helper inlines to are resolved; they reached
  planning as `SubLink is not supported yet`.
- A join or filter on `(ss.x).x` / `.n` over a subquery's
  `_pg_expandarray` column compares a column the subquery selects, which
  the join planner hashes; `information_schema.parameters` joined every
  function's arguments to `pg_type` pair by pair.
- Fixes RP-130 (`tests/backlog/test_rp_batch26.py::test_scenario[a long generate_series]`):
  a numeric or timestamp `generate_series` past 10,000,000 rows was cut off
  there without a word (`count(*)` said 10000000); it runs to its end, and
  is cancellable along the way. (The test pins the timestamp series; the
  numeric one is the same change and too slow for a debug-build test.)

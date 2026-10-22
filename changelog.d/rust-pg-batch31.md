### Rust PostgreSQL server: WHERE CURRENT OF, and a routine belongs to its schema

`UPDATE` and `DELETE ... WHERE CURRENT OF` change the row a cursor sits on.
Two schemas can each hold a function of one name, as in PostgreSQL 15.19.
A call, a DROP, an ALTER, a GRANT and a CALL find the routine by the schema
they name or, unqualified, by the search path. `DROP SCHEMA a, b` is one
deletion.

#### Added

- `UPDATE` / `DELETE ... WHERE CURRENT OF c`
  (`tests/backlog/test_rp_batch31.py::test_scenario[UPDATE and DELETE WHERE CURRENT OF]`,
  `[WHERE CURRENT OF a cursor it cannot use]`,
  `[WHERE CURRENT OF a cursor ordered by an index]` and
  `tests/backlog/test_rp_batch30.py::test_scenario[WHERE CURRENT OF a cursor not on a row]`).
  A cursor that is a simply updatable scan of one table keeps each row's
  stored id. That is a plain scan, one ordered by a B-tree index (an Index
  Scan, as PostgreSQL plans a cursor), or any FOR UPDATE cursor over the
  table. The statement changes that row, raising PostgreSQL's errors: 34000
  for a cursor that does not exist; 24000 for one not on a row, one that is
  not a simply updatable scan of the table, or one held from a committed
  transaction. RP-072 is down to `ALTER SYSTEM`, `ALTER TYPE ... ADD /
  RENAME ATTRIBUTE` and `CREATE TEXT SEARCH CONFIGURATION`.

#### Fixed

- Fixes RP-143 (`tests/backlog/test_rp_batch30.py::test_scenario[two schemas' routines of one name]`,
  `tests/backlog/test_rp_batch31.py::test_scenario[a call finds its routine by schema and search_path]`,
  `[DDL on a routine names it by schema]`,
  `[an unqualified CREATE FUNCTION lands on the search_path]` and
  `[a routine named in a schema that does not exist]`): a routine's
  catalog key carries its schema, so `create function b.h()` after `create
  function a.h()` is no longer 42723, and `b.h()` no longer calls `a.h()`.
  An unqualified call takes the first schema on the search path that holds
  the routine. An unqualified CREATE FUNCTION lands in the first schema on
  the search path, not always in `public` ("no schema has been selected to
  create in" when there is none). A routine named in a schema that does not
  exist is 3F000: `drop function nosuch.h(int)` used to drop another
  schema's `h(int)`. `ALTER FUNCTION ... SET SCHEMA` re-keys the routine
  and refuses one that already exists there (42723).
- Fixes RP-145 (`tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA of several schemas is one deletion]`
  and `[DROP SCHEMA names what its members' drops reach]`): `DROP SCHEMA a,
  b CASCADE` sends one notice, and its refusal is PostgreSQL's `cannot drop
  desired object(s) because other objects depend on them`, both naming the
  last schema's members first. Every schema named must exist before
  anything is dropped (IF EXISTS skips a missing one with a notice). The
  notice also names what the members' drops reach outside the schema, such
  as a view over one of its tables, instead of a second notice of its own.
- Fixes RP-146 (`tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA of a schema made in the open block]`):
  DROP SCHEMA of a schema the open block created drops it. The schema used
  to stay.
- Fixes RP-147 (`tests/backlog/test_rp_batch31.py::test_scenario[DROP FUNCTION IF EXISTS names the types as written]`):
  DROP FUNCTION / PROCEDURE IF EXISTS's notice names the argument types as
  the grammar wrote them (`nosuchfn(pg_catalog.int4,text)`), as
  PostgreSQL's does.
- Fixes RP-148 (`tests/backlog/test_rp_batch31.py::test_scenario[a procedure in DROP SCHEMA's notice]`):
  DROP SCHEMA's notice and refusal call a procedure a function, as
  PostgreSQL 15's `getObjectDescription` does.

#### Known

- RP-149, filed: a built-in is reached through any schema (`public.lower`
  answers where PostgreSQL is 42883)
  (`tests/backlog/test_rp_batch31.py::test_scenario[a built-in reached through a schema]`).
- RP-150, filed: DROP SCHEMA's refusal names the schema's members but not
  the objects that depend on them in turn
  (`tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA's refusal names what depends on its members]`).
- RP-151, filed: PL/pgSQL's FETCH, MOVE and CLOSE are refused, so a body
  cannot read the cursor it opens
  (`tests/backlog/test_rp_batch31.py::test_scenario[FETCH and CLOSE a cursor in PL/pgSQL]`).
- RP-137 now also covers DROP SCHEMA's notice, which lists a schema's
  members grouped by kind rather than in creation order
  (`tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA lists its members in creation order]`).

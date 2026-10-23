### Rust PostgreSQL server: schema-qualified built-ins, DROP SCHEMA's dependents, whole records in PL/pgSQL

#### Fixed

- Fixes RP-149 (`tests/backlog/test_rp_batch31.py::test_scenario[a built-in reached through a schema]`
  and `tests/backlog/test_rp_batch36.py::test_scenario[a built-in through a schema, anywhere in a statement]`):
  a built-in was reached through any schema, so `public.lower('A')` and
  `s.now()` answered. A built-in lives in `pg_catalog` alone, so through any
  other schema that holds no user function of its name it is now 42883
  (`function public.lower(unknown) does not exist`). That holds wherever
  in a statement the call is, `FROM` included.
- Fixes RP-150 (`tests/backlog/test_rp_batch31.py::test_scenario[DROP SCHEMA's refusal names what depends on its members]`
  and `tests/backlog/test_rp_batch36.py::test_scenario[DROP SCHEMA's refusal nests what depends on each member]`):
  DROP SCHEMA's refusal named only the schema's members. It now also names
  what depends on them in turn, such as a view elsewhere over a member
  table and a view over that view. Each member's dependents follow it,
  depth first, in the refusal and in the CASCADE notice, as PostgreSQL
  lists them.
- Fixes RP-154 (`tests/backlog/test_rp_batch31.py::test_scenario[a whole record in a PL/pgSQL expression]`
  and `tests/backlog/test_rp_batch36.py::test_scenario[a whole record in PL/pgSQL expressions]`):
  a whole record in a PL/pgSQL expression (`raise notice '%', r`,
  `r::text`, `r = s`, `r is null`) was 42703. It is now a value of the
  record's row type, and NULL for a record nothing has assigned.
- Fixes RP-174 (`tests/backlog/test_rp_batch36.py::test_scenario[an information_schema helper names its column]`):
  a select-list call of an `information_schema` helper
  (`_pg_char_max_length`) named its column `case`, after the body it is
  inlined to. It now takes the function's name, as in PostgreSQL.

#### Known

- RP-172, filed: `CREATE EXTENSION ... SCHEMA s` installs the extension in
  `public`
  (`tests/backlog/test_rp_batch36.py::test_scenario[an extension installed in a schema]`).
- RP-173, filed: a user function named like a built-in (`public.lower(int)`)
  takes calls the built-in should, `pg_catalog.lower('Q')` included. That's
  a wrong answer, not an error
  (`tests/backlog/test_rp_batch36.py::test_scenario[a user function of a built-in's name]`).

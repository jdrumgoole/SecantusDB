### Rust PostgreSQL server: an extension's objects, and a type's name, follow the schema

An extension now lives in the schema it is installed in, and the search
path decides what reaches it, as in PostgreSQL 15.19. The same goes for a
user type's unqualified name. Types, domains and extensions can move with
`SET SCHEMA`.

#### Fixed

- Fixes RP-172
  (`tests/backlog/test_rp_batch36.py::test_scenario[an extension installed in a schema]`,
  `tests/backlog/test_rp_batch38.py::test_scenario[an extension's functions follow the search path]`,
  `[an extension's type is its schema's]` and `[an extension off the path]`):
  `CREATE EXTENSION ... SCHEMA s` put everything in `public`. Now:
  - **Types.** The extension's types are `s`'s, as `pg_type` and
    `pg_extension.extnamespace` report.
  - **Functions.** A function (`similarity`, `digest`, `akeys`, ...) is
    found unqualified only while the search path reaches `s`, and
    qualified only by `s`. `pg_catalog.similarity(...)` is 42883, as is
    `similarity(...)` with `s` off the path.
  - **Operators.** citext's comparison and pg_trgm's operators apply only
    while `s` is on the path; otherwise a citext compares as text, as it
    does on PostgreSQL.
  - **Errors.** A missing schema is 3F000.
- Fixes RP-176 (`tests/backlog/test_rp_batch38.py::test_scenario[a user type is found through the search path]`):
  an unqualified user type name ignored the search path. With
  `search_path = s, public`, `'a'::mood` and `create table t (m mood)` were
  42704 for `s.mood`. A `public` type also stayed visible with `public` off
  the path. A name now resolves to the first schema on the path holding it.
- Fixes RP-177 (`tests/backlog/test_rp_batch38.py::test_scenario[ALTER EXTENSION SET SCHEMA]`
  and `[ALTER TYPE and ALTER DOMAIN SET SCHEMA]`): `ALTER TYPE`, `ALTER
  DOMAIN` and `ALTER EXTENSION ... SET SCHEMA` were refused.
  - **Columns.** They now move the type, along with every column, array
    column, field and signature that names it.
  - **Errors** are PostgreSQL's: 3F000 for a missing schema, 42710 `type
    "m" already exists in schema "s"`, and 0A000 for `plpgsql`.
- Fixes RP-179 (`tests/backlog/test_rp_batch38.py::test_scenario[DROP SCHEMA CASCADE drops its domains]`):
  `DROP SCHEMA s CASCADE` left `s`'s domains behind, unlisted in its
  notice, so a later `CREATE DOMAIN s.d` was 42710.
- Fixes RP-181 (`tests/backlog/test_rp_batch38.py::test_scenario[an array of a schema's type is a column]`):
  a column of an array of another schema's type (`s.m[]`) was missing from
  `pg_attribute`, so `\d` did not list it.

#### Known

- RP-178, filed: citext's own functions (`citext(text)`,
  `regexp_match(citext, ...)`, ...) are 42883. And `OPERATOR(s.=)` on two
  citexts compares as text while `s` is off the path
  (`tests/backlog/test_rp_batch38.py::test_scenario[citext's functions and an operator named by its schema]`).
- RP-180, filed: `ALTER TYPE s.nosuch ...` names the missing type
  `nosuch` rather than `s.nosuch`
  (`tests/backlog/test_rp_batch38.py::test_scenario[a missing type named with its schema]`).

### Rust PostgreSQL server: an extension's objects in the catalogs, built-in functions' signatures and definitions

An extension's functions, types, operators, casts and operator classes are
listed in the catalogs, each a member of its extension. A built-in
function's signature and definition print as PostgreSQL's do.

#### Added

- An extension's member objects (RP-105). `CREATE EXTENSION` of citext,
  hstore, pg_trgm, pgcrypto, btree_gin or btree_gist copies into the
  database the rows of each object its script makes:
  - its functions, types, operators, casts and operator classes and
    families, and its families' operators and support functions;
  - as PostgreSQL 15.19 leaves them in a fresh database
    (`pg15_extension_rows.txt`, written by
    `tools/probes/pg15_extension_rows_dump.py`);
  - each object with an oid of the database's own. A type the extension
    already made here keeps its oid, and a new type's array follows this
    server's rule.

  Each object is the extension's `e` member in `pg_depend`, and resolves
  as a `regproc`, `regprocedure` and `regtype`. `DROP EXTENSION` takes the
  rows with it. plpgsql's members (its handler, inline handler, validator
  and language) are listed too. Across the six extensions, 670 members
  describe as PostgreSQL's do.

  Fixes RP-105:
  - tests/backlog/test_rp_batch45.py::test_scenario[an extension's members]
  - tests/backlog/test_rp_batch57.py::test_scenario[every extension's members, described]
  - tests/backlog/test_rp_batch57.py::test_scenario[an extension's members go with it]

#### Fixed

- `pg_get_function_arguments`, `pg_get_function_identity_arguments` and
  `pg_get_function_result` of a built-in or an extension's function answer
  PostgreSQL 15.19's text (`pg15_function_sigs.tsv`, written by
  `tools/probes/pg15_function_sigs_dump.py`); they were NULL. Fixes RP-228
  (tests/backlog/test_rp_batch57.py::test_scenario[every built-in function's signature]).
- `pg_get_functiondef` of a built-in or an extension's function answers
  PostgreSQL's definition (`pg15_function_defs.tsv`), and of an aggregate
  42809, `"count" is an aggregate function`; it was NULL. Fixes RP-229
  (tests/backlog/test_rp_batch57.py::test_scenario[a built-in function's definition]).
- An operator class or family hides another of its name only within its
  access method, as `OpclassIsVisible` decides, so btree_gin's
  `int4_ops` describes unqualified. Fixes RP-230
  (tests/backlog/test_rp_batch57.py::test_scenario[an operator class hides another only within its access method]).
- A public function that a built-in of its name and arguments hides
  prints qualified as a `regprocedure` (`public.gen_random_uuid()`). Fixes
  RP-231 (tests/backlog/test_rp_batch57.py::test_scenario[a public function a built-in of its name hides prints qualified]).
- A reserved word as a `regprocedure`'s argument type (`count(any)`) is
  15.19's 42601 at the word; it was 42883. Fixes RP-232
  (tests/backlog/test_rp_batch57.py::test_scenario[a regprocedure's argument that is a reserved word]).

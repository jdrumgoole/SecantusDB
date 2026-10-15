### Rust PostgreSQL server: backlog batch 5

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- A `char(n)` field too long for a row type is placed at the text literal
  it came from (`'(abcd,x)'::t`, 22001 at 8), and a RECORD cast to the type
  truncates it as an explicit cast does (`row('abcd', 'x')::t` is
  `(abc,x)`), where it had been refused. Fixes RP-032
  (`tests/backlog/test_rust_postgres_gaps.py::test_row_type_char_overflow_has_a_position`).

- A cancel that lands while a large statement is still being PARSED is
  honoured at once, as PostgreSQL honours it (57014 0.2 s after a cancel
  0.5 s into a 1.2 MB statement, where it had waited for the 2.4 s parse).
  A statement over 64 KiB is parsed on a thread of its own while the
  planner polls the backend's cancel flag (`secantus_pgplan::cancel`),
  through every pass that parses it first (the table locks, the privilege
  check); the flag is only looked at there, and a parse it cut short is not
  memoised. A Query message's cycle now opens at its first byte (a
  `MessageStartHook` in the vendored pgwire), so a cancel sent while a long
  message is still arriving is kept rather than cleared as stale, and a
  57014 carries no position, which PostgreSQL never gives it and which
  took a 1.2 MB scan to look for. Fixes RP-002
  (`tests/backlog/test_rp_triage.py::test_cancel_during_parse_is_honoured_at_once`).

- `create type int` (and `integer`, `smallint`, `bigint`, `real`, `double
  precision`, `boolean`, `character varying`, `decimal` and the other
  grammar spellings with no `pg_type` row) creates `public."int"` beside the
  built-in, as PostgreSQL 15.19 does, where it was 42710. A bare `int` still
  means `int4`; `"int"` and `public."int"` reach the new type, which prints
  `"int"`. The type is held under its quoted spelling, as `"char"` is
  (`pgtypes::user_type_key`). Fixes RP-038
  (`tests/backlog/test_rust_postgres_gaps.py::test_create_type_named_int`).

- A 42704 for an unknown type is placed at a qualified name's first part
  (`'a'::public.nosuch` at 13) and at the literal a `regtype` reads
  (`'nosuch'::regtype` at 8), as PostgreSQL places them. Part of RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_unknown_type_error_position`).

- `DROP TYPE` over an enum, composite or range checks what depends on its
  values, as PostgreSQL does: columns of the type or of its array type, a
  domain over it and the domain's columns, and fields of composite types.
  Without CASCADE it is 2BP01 naming each in PostgreSQL's order; with it,
  they are dropped under one notice. The type had been dropped from under
  its columns, which kept their values typed as a type that no longer
  existed, and CASCADE left them. Part of RP-041
  (`tests/backlog/test_rust_postgres_gaps.py::test_drop_type_cascade_names_the_column`).
- `DROP DOMAIN ... CASCADE` over two columns of one table dropped only the
  first: the second read back as NULL. It drops every column, in one
  `ALTER` per table, and sends PostgreSQL's `drop cascades to` notice,
  which it had not sent at all. The extension drop that removes a type's
  columns had the same per-column loop, and is fixed with it
  (`::test_drop_domain_cascade_drops_every_column`).

#### Added

- The language validators `fmgr_internal_validator`, `fmgr_c_validator`,
  `fmgr_sql_validator` and `plpgsql_validator`, and
  `postgresql_fdw_validator`, transcribed from PostgreSQL 15
  (`CheckFunctionValidatorAccess`, foreign.c): `void` for a function of the
  validator's language, 42501 naming the language's own validator
  otherwise, 42883 for an oid naming no function, and the libpq options a
  foreign server or user mapping may set. Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[fmgr-validator]`,
  `[fdw-validator]`).
- Corpora `row_type_char_overflow`, `fmgr_validators`, `type_named_like_grammar` and `drop_type_dependents`.

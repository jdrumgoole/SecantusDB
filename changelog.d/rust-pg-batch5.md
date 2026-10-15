### Rust PostgreSQL server: backlog batch 5

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- A `char(n)` field too long for a row type is placed at the text literal
  it came from (`'(abcd,x)'::t`, 22001 at 8), and a RECORD cast to the type
  truncates it as an explicit cast does (`row('abcd', 'x')::t` is
  `(abc,x)`), where it had been refused. Fixes RP-032
  (`tests/backlog/test_rust_postgres_gaps.py::test_row_type_char_overflow_has_a_position`).

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
- Corpora `row_type_char_overflow` and `fmgr_validators`.

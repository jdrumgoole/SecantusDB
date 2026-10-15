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

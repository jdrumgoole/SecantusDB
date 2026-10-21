### Rust PostgreSQL server: Parse analyses the statement, and the wire messages come in PostgreSQL's order

The Rust PostgreSQL server now analyses a statement when the client sends
Parse, as PostgreSQL does, so a syntax error, an unknown table, column,
function or type, or a parameter whose uses conflict is the Parse's error
rather than surfacing later at Describe or Execute. Parameters declared
`unknown` take their type from where they are used, and one nothing resolves
is described as `text`.

A batch of smaller wire-protocol differences from PostgreSQL 15.19, found by
running CockroachDB's pgtest corpus, is closed with it: an aborted
transaction refuses Parse and Bind, a second Bind to an open portal is
refused, ParameterStatus arrives after the command it reports on, and
several values and row counts now match.

#### Fixed

- `secantus-pg`: Parse and Bind in an aborted transaction block answer 25P02,
  except for COMMIT, ROLLBACK, ROLLBACK TO and PREPARE TRANSACTION. Fixes
  RP-079 (`tests/backlog/test_rp_found_pgtest.py::test_parse_in_aborted_transaction`).
- `secantus-pg`, `secantus-pgwire`: Parse runs the describe analysis (SELECT,
  INSERT, UPDATE, DELETE, MERGE; syntax for every statement) with the
  parameter types inferred from their uses, and reports its class-42 and
  22P02 errors there; parameters declared `unknown` (705) are resolved from
  context; an unresolved parameter describes as `text`. Fixes RP-080
  (`tests/backlog/test_rp_found_pgtest.py::test_unknown_parameters_take_their_column_types`,
  `::test_conflicting_parameter_uses_fail_at_parse`,
  `::test_declared_parameter_type_is_checked_at_parse`) and RP-090
  (`::test_unknown_type_fails_at_parse`).
- `secantus-pg`, `secantus-pgwire`: ParameterStatus for a changed setting is
  sent after CommandComplete, just before ReadyForQuery, and the startup
  ParameterStatus messages come in PostgreSQL's fixed order. Fixes RP-081
  (`::test_set_reports_after_command_complete`, `::test_startup_parameter_order`).
- `secantus-pg`: RowDescription reports a type size of 4 for void, enums and
  the reg* types, and 8 for money and xid8. Fixes RP-082
  (`::test_fixed_length_types_report_their_size`).
- `secantus-pg`: a BEGIN inside a transaction block gives WARNING 25001 and
  leaves the block's savepoints alone. Fixes RP-084
  (`::test_begin_inside_a_transaction_warns`).
- `secantus-pg`: `0::"char"` is the zero byte in binary. Fixes RP-085
  (`::test_char_of_zero_is_a_zero_byte`).
- `secantus-pgwire`: `COPY ... TO STDOUT ... HEADER` no longer counts the
  header line in its `COPY n` tag. Fixes RP-086 (`::test_copy_header_is_not_a_row`).
- `secantus-pgplan`: an empty jsonpath is 22P02. Fixes RP-088
  (`::test_empty_jsonpath_is_a_syntax_error`).
- `secantus-pg`: binding to a named portal still open in the transaction is
  42P03 and aborts the block. Fixes RP-089 (`::test_bind_to_an_existing_portal`).
- `secantus-pgwire`: a missing unnamed portal is reported as `portal ""`.
  Fixes RP-091 (`::test_unknown_portal_error_names_it`).

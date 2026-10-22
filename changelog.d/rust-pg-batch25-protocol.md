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
- `secantus-pg`: Bind checks its shape (parameter and result-format counts,
  a cached plan whose result changed) and holds every binary parameter to
  its type's receive function (`binary_recv.rs`), all before BindComplete.
  Fixes RP-078 (`::test_bind_decodes_parameters`, `::test_bind_checks_its_shape`,
  `::test_cached_plan_fails_at_bind`).
- `secantus-pg`: binary reg* parameters are read through their oid cast and
  regclass / regtype parameters keep their types. Fixes RP-083
  (`::test_reg_parameters_render_as_text`).
- `secantus-pgplan`: a view's columns name the view in RowDescription, and a
  subquery's, a CTE's and a join's their base columns. Fixes RP-087
  (`::test_view_columns_name_their_base_columns`).
- `secantus-pgplan`, `secantus-pg`: `IntervalStyle` (all four styles, and the
  default style's `+` after a negative part), `bytea_output = escape`,
  `extra_float_digits` below 1, interval input `@ ... ago` and
  `sql_standard`'s leading minus; `SET application_name` shows in
  `pg_stat_activity`. Fixes RP-068
  (`tests/backlog/test_rp_found_2026_10_20.py::test_output_settings`,
  `tests/backlog/test_rp_found_pgtest.py::test_extra_float_digits_below_one`).
- `secantus-pgplan`: recursive CTE `SEARCH DEPTH FIRST` / `BREADTH FIRST` and
  `CYCLE`, rewritten as PostgreSQL's rewriter does; they were dropped and a
  CYCLE never stopped. Fixes RP-062 (`::test_recursive_search_and_cycle`).
- `secantus-pgplan`: a recursive CTE read under a plain `LIMIT` runs only as
  far as it is read; UNION deduplication is linear; the invented
  1,000,000-row 54000 is gone. Fixes RP-063 (`::test_recursive_cte_under_limit`).
- `secantus-pg`: temporary tables' `ON COMMIT DELETE ROWS` and `ON COMMIT
  DROP`, and 42P16 for ON COMMIT on a permanent table. Fixes RP-065
  (`::test_temp_table_on_commit`).
- `secantus-pgplan`: float8 overflow and underflow are 22003 in `+ - * /` and
  in the numeric-to-float8 cast. Fixes RP-066 (`::test_float8_out_of_range`).
- `secantus-pgplan`, `secantus-pg`: every COPY option PostgreSQL 15 takes --
  DELIMITER, NULL, QUOTE, ESCAPE, FORCE_QUOTE, FORCE_NOT_NULL, FORCE_NULL,
  FREEZE and ENCODING -- checked, written and read as PostgreSQL does. All
  but FORMAT and HEADER were refused, DELIMITER and NULL included. Fixes
  RP-073 (`::test_copy_options`).
- `secantus-pgplan`, `secantus-pg`: named and defaulted arguments to the
  built-ins (`pg_proc_argnames.tsv`, from PostgreSQL 15.19's `pg_proc`);
  `pg_column_size` modelled on the datum layouts, for literals and stored
  columns; `CASE` columns named `case` and rewritten expressions keeping
  their names; `REFRESH ... CONCURRENTLY`'s refusals; the 42622
  identifier-truncation NOTICE. Fixes RP-074 (`::test_smaller_answers`).
- `secantus-pgplan`: a strict comparison with a NULL constant operand is NULL
  before the statement runs, as `eval_const_expressions` folds it. Fixes
  RP-075 (`tests/backlog/test_rp_found_2026_10_21.py::test_strict_null_folding`).
- `secantus-pgplan`: NULLIF takes the left input type of the `=` operator
  PostgreSQL resolves. Fixes RP-076 (`::test_nullif_result_type`).
- `secantus-pg`: the duplicate check behind a new unique index or primary key
  sorts the keys rather than comparing every pair: `pgbench -i -s 1` took
  440 s (429 s of it the primary keys), now 20 s.

- `secantus-pg`, `secantus-pgplan`: `ALTER DEFAULT PRIVILEGES` (FOR ROLE,
  IN SCHEMA; tables, sequences, functions, types, schemas) is applied to the
  objects those roles create, and shown in `pg_default_acl`; a role's
  `CONNECTION LIMIT` is enforced at login (53300). Fixes RP-067
  (`tests/backlog/test_rp_found_2026_10_20.py::test_default_privileges_and_connection_limit`).
- `secantus-pg`, `secantus-pgplan`: `CREATE DATABASE` takes its options --
  TEMPLATE (the template's tables, rows and indexes copied), OWNER,
  ENCODING, LOCALE / LC_COLLATE / LC_CTYPE, CONNECTION LIMIT, IS_TEMPLATE,
  ALLOW_CONNECTIONS -- with `createdb`'s checks, and `ALTER DATABASE`
  renames, changes the owner and sets those options; a database's
  connection limit is enforced at login; `pg_database.datacl` is
  `aclitem[]`. RP-064 stays open for what a non-UTF8 encoding does inside
  its database (`tests/backlog/test_rp_found_2026_10_20.py::test_create_database_options`
  now pins the rest).

More regression tests for these, past each item's gap test, are in
`tests/backlog/test_rp_batch25.py`.

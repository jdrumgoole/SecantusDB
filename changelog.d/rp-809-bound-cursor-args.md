### Rust PG server: a bound cursor's arguments are bound at OPEN

A PL/pgSQL cursor declared with arguments, such as `c cursor (k int) for
select a from t where a = k`, could not be opened. `open c(5)` failed with
42703 `column "k" does not exist`, because the OPEN never read its argument
list and planned the query with no `k` in scope. When a block variable had
the same name as the argument, the query read that variable instead, which
gave a wrong answer.

PostgreSQL 15.19 compiles a cursor's arguments as variables in a namespace
level of their own, labelled with the cursor's name. `exec_stmt_open` assigns
them by running the OPEN's argument list as a `SELECT ... INTO` the cursor's
argument row, and then opens the query over them. This server now does the
same:

- Positional, named and mixed notation all bind.
- Each value moves in by the assignment rule: rounding, a length, a domain's
  CHECK.
- `c.k` names an argument, and an argument hides a block variable of the same
  name.
- FOUND and ROW_COUNT are set as a SELECT INTO sets them.
- An error raised while computing an argument carries PostgreSQL's
  `SQL expression` CONTEXT frame. A literal's input error carries no frame,
  as in PostgreSQL.

#### Fixed

- `secantus-pg` (`plpgsql_fn/open_cursor.rs`, `plpgsql_fn.rs`): the OPEN of a
  bound cursor reads `cursor_explicit_argrow` and the statement's `argquery`,
  assigns the argument row, and binds the cursor's query with the arguments
  in an innermost level labelled with the cursor's name.
  Fixes RP-809 (tests/backlog/test_rp_found_rp807.py::test_bound_cursor_arguments_are_bound,
  tests/backlog/test_rp_found_rp807.py::test_bound_cursor_argument_errors_are_framed).

#### Found while working it

- RP-817: a FOR loop over a bound cursor is refused as a gap (0A000).
- RP-818: a variable declared `%TYPE` cannot be assigned (3F000 / 42601).

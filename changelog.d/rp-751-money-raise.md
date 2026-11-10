### Rust PG server: a PL/pgSQL RAISE prints each value by its type's output function

A RAISE printed its parameters, and its `USING` DETAIL, HINT and MESSAGE
options, from the value the server carries internally rather than the value's
text. A `money` variable printed as `5.25` where PostgreSQL 15.19 prints
`$5.25` (`cash_out`).

The interpreter now prints each value by its type's output function, as
PostgreSQL's `convert_value_to_string` does. It finds the type the way a
Describe of `SELECT <expression>` would. A boolean is still `t` / `f`, and a
NULL parameter is still `<NULL>`.

A NOTICE, WARNING or INFO raised from a block with a DECLARE also dropped its
DETAIL and HINT. They are now sent at every level, as `exec_stmt_raise` passes
them to `ereport`.

#### Fixed

- `secantus-pg` (`plpgsql_fn.rs`): `eval_output` evaluates a RAISE
  parameter or option and prints it with `typeout::output_text` over the
  expression's described type.
  Fixes RP-751 (tests/backlog/test_rp_found_rp712.py::test_money_variable_prints_through_cash_out).
- `secantus-pg` (`plpgsql_fn.rs`, `lib.rs`): `Host::notice` takes the
  DETAIL and HINT.
  Fixes RP-832 (tests/backlog/test_rp_found_rp751.py::test_a_notice_keeps_its_detail_and_hint).

#### Found while working it

- RP-833: a RAISE below ERROR carries no CONTEXT. PostgreSQL frames it
  `PL/pgSQL function inline_code_block line 1 at RAISE`.

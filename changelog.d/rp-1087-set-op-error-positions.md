### A set operation's type errors point at the column PostgreSQL points at

When the two sides of a `UNION`, `INTERSECT` or `EXCEPT` have column types
that cannot be matched, the Rust PostgreSQL server now places the error
exactly where PostgreSQL 15 does: at the right-hand column of the first
pair that does not match, in the first set operation that fails, working
the left side before the right. Before, it guessed the top operation's
right side's first column, which was the wrong column (`select 1, 'a'::text
union select 2, 3`), the wrong arm (`select 1 union select 'a'::text union
select 2`) or no position at all (a parenthesised right side).

The same column locations now also place two errors that had no position:
`each UNION query must have the same number of columns`, and `UNION could
not convert type oid to numeric`.

#### Fixed

- `secantus-pgplan` / `secantus-pg`: the planner records each leaf side's
  column locations as `exprLocation` finds the analysed target (a cast of
  an untyped literal at the literal, a `*` at its column reference, a
  `VALUES` side nowhere), and the executor hands a nested set operation's
  column up at the expression its common type came from, as
  `transformSetOperationTree` does, recording the location of the failing
  column for the 42804, the 42601 column count and the 42846 conversion.
  Fixes RP-1087 (tests/backlog/test_rp_found_rp1050.py::test_set_op_mismatch_column_and_arm),
  with more positions pinned in
  tests/test_rust_pg_set_op_positions.py::test_set_op_mismatch_position and
  tests/test_rust_pg_set_op_positions.py::test_set_op_column_count_and_conversion_positions.
- Filed RP-1176: the same errors name an array type `_int4` where
  PostgreSQL says `integer[]`.

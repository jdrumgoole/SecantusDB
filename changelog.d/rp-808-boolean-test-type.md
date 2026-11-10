### Rust PG server: `IS TRUE` and its kin refuse an argument that is not boolean

`IS [NOT] TRUE / FALSE / UNKNOWN` accepted a value of any type and returned
an answer. `select 1 is true` was `false` and `null::int is unknown` was
`true`. A WHERE `x is true` over an `int` column silently matched no rows.
`update t set b = (i is not false)` wrote `coalesce(i, true)` into every
row, and `select ('x' || 'y') is true` returned `xy`. PostgreSQL 15.19
refuses each one with 42804 `argument of IS TRUE must be type boolean, not
type integer`, because `transformBooleanTest` coerces the argument to
boolean under the test's name.

The test is now checked when the statement is analysed, as `AND` / `OR` /
`NOT` already were, so a typed non-boolean argument is refused whether or
not a row reaches it. A value that analysis cannot type (an untyped `||`, a
COALESCE) is refused when the test is evaluated. The error is reported where
PostgreSQL's `exprLocation` puts it: at an operator expression's leftmost
token, and at the keyword of a COALESCE, GREATEST, LEAST or CASE. An
untyped literal is still read as a boolean (`'yes' is true` is `true`, and
`'a' is true` is 22P02).

#### Fixed

- `secantus-pgplan` (`optype.rs`): the expression walk checks a
  `BooleanTest`'s argument (`boolean_test_argument`).
- `secantus-pgplan` (`lib.rs`): `boolean_test_value` evaluates the operand
  once and refuses a non-boolean value. It replaces the rewrite to
  `coalesce(x, false)` / `x IS NULL`, which accepted any type.
- `secantus-pgplan` (`srf_place.rs`): `leftmost` places a COALESCE,
  GREATEST / LEAST or CASE at its keyword, which also moves a `WHERE
  coalesce(i, 0)` error to where PostgreSQL reports it.
- Fixes RP-808 (tests/backlog/test_rp_found_rp774.py::test_a_boolean_test_over_a_non_boolean_is_42804, tests/backlog/test_rp_found_rp808.py::test_a_boolean_test_over_a_non_boolean_is_42804_at_its_argument)

#### Known gaps

- An UPDATE's SET expression is judged only when a row reaches it, so
  `update t set b = (i is true) where false` is still `UPDATE 0` (RP-810).
- A CHECK constraint's expression is not coerced to boolean (RP-811).
- `AND` / `OR` / `NOT` still report a non-boolean argument at the operator
  rather than at its leftmost token (RP-812).

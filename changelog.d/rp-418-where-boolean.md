### Rust PG server: a WHERE or HAVING that is not boolean is 42804, not an empty answer

PostgreSQL's `coerce_to_boolean` refuses a WHERE or HAVING clause of any type
but boolean at parse analysis. Here a typed clause of another type, such as
`where a::text`, `where null::int`, `where a` over an integer column or
`having count(*)`, was lowered as it stood. It matched no row, so the statement
answered an empty result where PostgreSQL 15.19 answers an error.

#### Fixed

- `secantusd-pg`: a WHERE in a SELECT (a subquery's included), an UPDATE or
  a DELETE, and a HAVING, whose type is a built-in type other than boolean
  answers 42804 `argument of WHERE must be type boolean, not type text`
  (`HAVING` for a HAVING) at the clause's leftmost token, as PostgreSQL
  15.19 does. The check runs after the clause's own expression is analysed,
  in PostgreSQL's order.
- An error placed at an expression's leftmost token now places a NULLIF at
  its keyword, not at its first argument, as PostgreSQL's `exprLocation`
  does.

Fixes RP-418 (tests/backlog/test_rp_where_types.py::test_where_must_be_boolean,
tests/backlog/test_rp_where_types.py::test_clause_must_be_boolean_everywhere).

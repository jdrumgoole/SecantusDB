### Rust PG server: CREATE FUNCTION holds a SQL body to its declared result

A `LANGUAGE sql` function declared to return `integer` whose body ended in
`select 'abc'`, or two columns, or an INSERT with no RETURNING, was created
anyway, and so was a body with `select 'x'::int` or a CALL of a procedure
that does not exist. Each error waited for the first call. PostgreSQL
refuses all of them at CREATE (`fmgr_sql_validator`).

#### Fixed

- The final statement is held to the declared result, as
  `check_sql_fn_retval` does. A 42P13 `return type mismatch in function
  declared to return ...` carries PostgreSQL's DETAIL: the actual return
  type, `Final statement must return exactly one column.`, too few or too
  many columns, a column's type, or `Function's final statement must be
  SELECT or INSERT/UPDATE/DELETE RETURNING.`. It also carries CONTEXT `SQL
  function "f"`. The check covers scalar, SETOF, RETURNS TABLE and OUT
  results, composite types and tables' row types.
- A coercion PostgreSQL would insert passes. It accepts any implicit or
  assignment cast in `pg_cast` (numeric to integer, integer to bigint), a
  user cast, any value into a string type, elements of arrays, and a
  record or a composite as a whole row. A type this server cannot place
  is not judged, so no function PostgreSQL accepts is refused.
- A string constant a cast reads (`'x'::int`, `int4 'x'`, `cast('x' as
  int)`) is read by its type's input function at CREATE. This holds even
  where the expression never runs (`case when false then 'x'::int end`),
  as `coerce_type` reads it during analysis. A typmod and a domain's
  constraints wait for the call, as there. The 22P02 / 22003 / 42P01
  (`'nosuchrel'::regclass`) points at the constant. So does an operator's
  string operand that planning folds (`'a' = 1`, `a = 'q'`, `1 in
  ('z')`).
- A CALL in a body resolves its procedure at CREATE. A missing one is
  42883 `procedure p(integer, unknown) does not exist`, and a function is
  42809 `f() is not a procedure`, each at the name. A string constant for
  an `int` parameter is read as one.
- A SQL-standard body (`BEGIN ATOMIC ... END`, `RETURN`) is analysed with
  `check_function_bodies` off, as PostgreSQL analyses it while it is
  parsed. Only the result check waits for the setting.

Fixes RP-500 (tests/backlog/test_rp_function_bodies.py::test_create_checks_literals_and_result,
tests/backlog/test_rp_function_bodies.py::test_validator_checks).
Found while working it, and filed:
- RP-507: a constant whose type comes from an operator over a column or a
  parameter, an INSERT or UPDATE target, or COALESCE is not read at
  CREATE.
- RP-508: DROP FUNCTION / PROCEDURE without arguments over the other kind
  is 42809, and DROP ROUTINE's notice says "function".
- RP-509: a CALL that leaves a defaulted parameter out, names its
  arguments or passes a column fails.
- RP-510: a malformed range literal's 22P02 has no DETAIL.
- RP-511: PREPARE evaluates a constant expression (`1/0` is 22012) and
  does not read a cast constant it does not evaluate.

### Rust PG server: CREATE FUNCTION checks a SQL function's body

A `LANGUAGE sql` function whose body named a missing table or column was
created anyway, and the error waited for the first call. PostgreSQL checks
the body when the function is created, because `check_function_bodies` is on
by default, so a migration that defined a broken function failed there and
not here.

#### Fixed

- With `check_function_bodies` on, CREATE FUNCTION and CREATE PROCEDURE now
  analyse each query and write in a `LANGUAGE sql` body against the catalog
  as it stands, as `fmgr_sql_validator` does. Nothing in the body runs: no
  function it calls, no sequence, no subquery. A missing relation, column or function, an ambiguous column, or an
  operator with no match is the CREATE's error. Its position points into
  the CREATE's text, as `match_prosrc_to_query` places it: through a
  dollar-quoted or quoted body (doubled quotes counted), past earlier
  statements, and around parameters bound by name.
- As in PostgreSQL:
  - A body may call its own function.
  - A body with a polymorphic argument is not analysed.
  - A utility statement's names are resolved only when it runs, so a body
    that creates a table and then reads it is refused.
  - An error only execution raises (a division by zero) waits for the call.
  - `SET check_function_bodies = off` (as `pg_dump` writes) turns the check
    off.
- A refused CREATE leaves no routine behind, and a refused CREATE OR
  REPLACE keeps the routine it would have replaced.

Fixes RP-465 (tests/backlog/test_rp_function_bodies.py::test_create_checks_the_body,
tests/backlog/test_rp_function_bodies.py::test_create_checks_more_bodies,
tests/backlog/test_rp_function_bodies.py::test_refused_create_leaves_no_routine,
tests/backlog/test_rp_function_bodies.py::test_create_runs_nothing_in_the_body).
Found while working it, and filed:
- RP-500: the validator's other checks (a literal's input, the result type,
  a CALL, a SQL-standard body with the setting off).
- RP-501: a VALUES list's column reference is refused as a gap, and a CTE no
  statement reads is not analysed.
- RP-502: a SQL function's array parameter cannot be subscripted.

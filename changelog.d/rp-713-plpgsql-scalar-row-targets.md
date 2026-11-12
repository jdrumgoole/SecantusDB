### PL/pgSQL loops and dynamic queries fill plain variables

A PL/pgSQL `FOR` over a query can now name ordinary variables as its loop
target, as PostgreSQL allows: `for x, y in select id, name from t loop`
assigns each column to its variable on every row, converted to the
variable's type, and `execute 'select ...' into x, y` does the same for a
dynamic query. Both used to be refused as not supported unless the target
was a record or a row-typed variable. A `FOR` over a query that returns no
rows now leaves its target NULL, as PostgreSQL's does.

#### Fixed

- `FOR x IN SELECT ...`, `FOR x, y IN SELECT ...`, `FOR ... IN EXECUTE`
  and `EXECUTE ... INTO x, y` over scalar variables were 0A000 `this FOR
  target` / `this EXECUTE INTO target ... is not supported yet`. The
  grammar makes a list of scalar variables an anonymous row with no datum
  number in the dump, which only a plain `SELECT ... INTO` handled; every
  INTO and FOR target now goes through the one path, which fills the row
  column by column by the assignment rule (a `varchar(3)` target is 22001,
  text `'7'` into an `int` is 7).
- A `FOR` over a query returning no rows left its target as it was;
  PostgreSQL's `exec_for_query` moves a NULL row into it.
- Each execution of a `SELECT ... INTO`, `FETCH ... INTO` or `EXECUTE ...
  INTO` naming scalar variables added an interpreter datum that was never
  freed, so such a statement in a loop grew the function's memory with
  every iteration; the anonymous row is now dropped after its assignment.

Fixes RP-713 (tests/backlog/test_rp_found_rp694.py::test_for_and_execute_into_scalar_variables);
pinned beside it by
tests/backlog/test_rp_found_rp694.py::test_scalar_targets_are_moved_as_postgresql_moves_them.

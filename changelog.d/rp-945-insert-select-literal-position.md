### An INSERT ... SELECT literal's domain error is placed at the literal

An untyped literal in a plain `INSERT ... SELECT` target list, written to
a composite or domain-array column, is now read as that column's type
while the statement is analysed, as PostgreSQL reads it: a field's domain
error is reported at the literal (`insert into tct(c) select '(0,x)'` is
23514 at 27), and it is raised even when the SELECT returns no rows.

#### Fixed

- `INSERT ... SELECT` with a bare string literal for a composite or
  domain-array column checks it in analysis, with its position, unless a
  set operation, ORDER BY, DISTINCT or GROUP BY resolves it to `text`
  first.
- Fixes RP-945 (tests/backlog/test_rp_found_rp552.py::test_insert_select_literal_domain_error_position)
- Filed RP-1044: a `text` SELECT column written to a composite column is
  accepted instead of 42804.

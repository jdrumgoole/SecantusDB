### A PL/pgSQL FOR loop or OPEN walks a DML statement's RETURNING rows

`for x in update t set a = a + 1 returning a loop` and `open c for insert
into t values (5) returning a` used to be 0A000 on the Rust PostgreSQL
server. As on PostgreSQL, where `SPI_cursor_open` takes any statement that
returns rows, the statement now runs and its RETURNING rows become the
portal. Without RETURNING it is 42P11 `cannot open UPDATE query as cursor`,
and a procedure's COMMIT inside a loop over such a portal is 55000, since
only a read-only portal can be held across it.

#### Fixed

- A PL/pgSQL FOR loop or OPEN over an INSERT, UPDATE or DELETE with
  RETURNING walks its rows; without RETURNING it is 42P11.
- A COMMIT or ROLLBACK inside a loop over such a portal is 55000 `cannot
  perform transaction commands inside a cursor loop that is not read-only`.
- Fixes RP-905 (tests/backlog/test_rp_found_rp828.py::test_a_dml_statement_opens_as_a_portal)

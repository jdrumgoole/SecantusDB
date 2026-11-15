### A PL/pgSQL function can FETCH from a refcursor parameter

A PL/pgSQL function that takes a `refcursor` and reads it with `FETCH`,
`MOVE` or `CLOSE` was refused at `CREATE FUNCTION` with 42601 `variable "c"
must be of type cursor or refcursor`, because the parser types a parameter
as `unknown`. Such a function is now created, and the call reads the
caller's cursor, as PostgreSQL does.

#### Fixed

- Rust PG server: `FETCH` / `MOVE` / `CLOSE` over a `refcursor` parameter
  in PL/pgSQL. Fixes RP-901 (tests/backlog/test_rp_found_rp744.py::test_a_refcursor_parameter_can_be_fetched)

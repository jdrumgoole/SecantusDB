### A PREPARE's error points into the PREPARE

When the query inside a `PREPARE` fails analysis, the Rust PostgreSQL server
now reports the error's position in the statement the client sent, as
PostgreSQL does. It planned a deparsed copy of the query, so the position was
an offset into that copy: `prepare p1(int) as select 12 = true` pointed at the
`(` of `p1(int)` rather than at the `=`.

#### Fixed

- Rust PG server: an analysis error in a `PREPARE`'s query is positioned in
  the `PREPARE` text.
  Fixes RP-593 (tests/backlog/test_rp_found_rp530.py::test_prepare_error_position)

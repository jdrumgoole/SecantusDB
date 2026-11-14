### An untyped literal beside a row is read as a record

Comparing a row with an untyped string literal, as in `select fs = 'x' from fs`, now fails the way PostgreSQL fails it: the operator resolves to `record = record`, and the literal cannot be read as an anonymous record, so the error is 0A000 `input of anonymous composite types is not implemented`, pointing at the literal. The same holds for `<`, `IS DISTINCT FROM` and `NULLIF`.

#### Fixed

- Rust PG server: a row beside an untyped string literal under a record operator is 0A000 `input of anonymous composite types is not implemented` at the literal, where it was `comparing document with string using = is not supported yet` with no position.
  Fixes RP-591 (tests/backlog/test_rp_found_rp530.py::test_untyped_literal_beside_a_row)

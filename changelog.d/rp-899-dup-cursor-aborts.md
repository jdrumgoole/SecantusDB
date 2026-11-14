### A refused DECLARE aborts its transaction block

Declaring a cursor under a name that is already open in the block is an
error, `42P03 cursor "c" already exists`, and like any error inside a
transaction block it now aborts the block: the next statement is refused
with 25P02 until the ROLLBACK, as on PostgreSQL. The Rust PostgreSQL server
used to report the error and let the block run on.

#### Fixed

- Rust PG server: a DECLARE refused for a duplicate cursor name (42P03), or
  over a statement it cannot open a cursor on (0A000), aborts the
  transaction block.
- Fixes RP-899 (tests/backlog/test_rp_found_rp744.py::test_a_duplicate_cursor_name_aborts_the_block).
  Regression test: tests/test_rust_pgserver_slice.py::test_a_duplicate_cursor_name_aborts_the_block

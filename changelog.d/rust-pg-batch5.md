### Rust PostgreSQL server: backlog batch 5

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- A `char(n)` field too long for a row type is placed at the text literal
  it came from (`'(abcd,x)'::t`, 22001 at 8), and a RECORD cast to the type
  truncates it as an explicit cast does (`row('abcd', 'x')::t` is
  `(abc,x)`), where it had been refused. Fixes RP-032
  (`tests/backlog/test_rust_postgres_gaps.py::test_row_type_char_overflow_has_a_position`).

#### Added

- Corpus `row_type_char_overflow`.

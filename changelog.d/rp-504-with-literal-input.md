### An unread WITH item's literal is read as its type

PostgreSQL reads an untyped string literal under a cast through its type's
input function while it analyses the statement, so `with w as (select
'x'::int) select 1` fails with `invalid input syntax for type integer` even
though nothing reads `w`. The Rust PostgreSQL server planned such an item
without running it and answered 1; it now reads each such literal in an
unread item, in the order written, and reports the first one refused at the
literal.

#### Fixed

- An unread WITH item's literal cast is read by its type's input function
  (22P02, 22008, ...), before any error written after it.
  Fixes RP-504, in part (tests/backlog/test_rp_fromless_analysis.py::test_with_unread_literal_input)

### An escape-string literal's input error is placed

`select E'a\tb'::int4` now reports its 22P02 at the literal, position 8, as
PostgreSQL 15.19 does. The error position was found by matching the value the
message quotes against each literal's spelling, and an escape-string literal
whose escapes decode to something else (a real tab for `\t`) never matched.
The literal's escapes are now decoded as the scanner decodes them before the
comparison, for plain casts and for `int2vector` / `oidvector` input alike.

#### Fixed

- Rust PG server: an input error from an `E'...'` literal holding an escape
  carries the literal's position.

Fixes RP-867 (tests/backlog/test_rp_found_rp605.py::test_escaped_literal_input_error_has_a_position)

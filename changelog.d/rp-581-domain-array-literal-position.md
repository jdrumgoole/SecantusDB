### A domain array literal's length error points at the literal

`select '{abcd}'::d[]`, with `d` a domain over `char(3)`, fails with
`value too long for type character(3)` as PostgreSQL does, and now
reports the literal's position with it, as PostgreSQL 15.19 does: its
`array_in` reads each element through `domain_in` while parsing the
literal.

#### Fixed

- A 22001 from a literal cast to an array of a domain over `char(n)`
  carries the literal's position.
  Fixes RP-581 (tests/backlog/test_rp_found_rp570.py::test_domain_array_literal_error_position)

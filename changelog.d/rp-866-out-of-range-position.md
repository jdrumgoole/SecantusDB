### A literal's out-of-range error points at the literal

`select '70000'::int2` now reports its 22003, `value "70000" is out of range
for type smallint`, at the literal (position 8), as PostgreSQL does; so does
an out-of-range number inside an `int2vector`, `oidvector` or array literal.
The error carried no position before.

#### Fixed

- An input function's 22003 for a constant is placed at the literal.
  Fixes RP-866 (tests/backlog/test_rp_found_rp605.py::test_out_of_range_literal_has_a_position)

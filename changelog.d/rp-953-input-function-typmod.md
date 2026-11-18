### A type's input function called with a typmod applies it

A type's input function called by name with its three arguments, such as
`numeric_in('1.5', 0, 5)` or `varcharin('abcdef', 0, 7)`, now applies the
modifier as PostgreSQL does: the value is read, then fitted to the type
with that modifier as a column assignment fits it, so an overlong string
is refused rather than cut and a numeric that does not fit overflows.
Before, every call with a modifier other than -1 was refused as not
supported.

#### Fixed

- `numeric_in`, `varcharin`, `bpcharin`, `bit_in`, `varbit_in`,
  `time_in`, `timetz_in`, `timestamp_in`, `timestamptz_in` and
  `interval_in` with a typmod (a constant or any expression) are
  rewritten to `__typmod_coerce`, matching PostgreSQL 15.19 on overflow,
  length, rounding and NULL.
- Fixes RP-953 (tests/backlog/test_rp_found_rp750.py::test_input_function_with_a_typmod)
- Filed RP-1042: a `numeric(0, s)` type name is not refused 22023.

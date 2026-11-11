### Rust PG server: a `numeric` cast to an integer type raises PostgreSQL's errors

A `numeric` cast to `smallint`, `integer` or `bigint` now fails the way
`numeric_int2` / `numeric_int4_opt_error` / `numeric_int8` do on
PostgreSQL 15.19, in plain SQL and in a PL/pgSQL assignment or `INTO`.

#### Fixed

- A value out of range is 22003 `integer out of range` (`smallint` /
  `bigint` for those casts), quoting no value; it was `integer out of
  range: "100000000000000000000"`, and a `::int2` cast said `integer`.
- `NaN` and `±Infinity` are 0A000 `cannot convert NaN to <type>` /
  `cannot convert infinity to <type>`; they were 22003 out-of-range errors.

Fixes RP-749 (tests/backlog/test_rp_found_rp712.py::test_numeric_out_of_int4_range_quotes_no_value),
pinned further by tests/backlog/test_rp_found_rp712.py::test_numeric_to_integer_errors_name_the_target_type
and the `secantus-pgplan` unit test `numeric_to_integer_errors_match_postgres`.

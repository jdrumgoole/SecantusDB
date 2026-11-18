### A `numeric` type name's precision and scale are checked when it is read

PostgreSQL checks `numeric(p, s)` as soon as the type name is read, so
`'1.5'::numeric(0,1)` -- and even `null::numeric(0)` -- is 22023 `NUMERIC
precision 0 must be between 1 and 1000`, placed at the type name. The Rust
PostgreSQL server ran the cast and answered with an overflow instead. A cast
now applies `numerictypmodin`'s checks: precision 1 to 1000, scale -1000 to
1000, and at most two modifiers.

#### Fixed

- A cast to `numeric` with an out-of-range precision or scale, or more than
  two modifiers, is 22023 at the type name, as on PostgreSQL 15.19.
  Fixes RP-1042 (tests/backlog/test_rp_found_rp750.py::test_numeric_precision_zero_is_refused)

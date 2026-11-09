### Rust PG server: an ARRAY sublink and `array_agg` keep their element arrays' lower bounds

`array(select f ...)` and `array_agg(f)` over arrays whose lower bound is
not 1 now answer as PostgreSQL 15.19 does: `[1:1][2:3]={{3,4}}`, the outer
dimension from 1 and the inner ones the first array's bounds
(`accumArrayResultArr`).

#### Fixed

- An ARRAY sublink (correlated or not), `array_agg` -- grouped, DISTINCT,
  ordered or as a window function -- over element arrays such as
  `'[2:3]={3,4}'::int[]` answered `{NULL}`, and as text the array of strings
  `{"[2:3]={3,4}"}`: each bounded element was kept whole inside the result
  instead of being accumulated into it.
  Fixes RP-731 (tests/backlog/test_rp_found_rp730.py::test_accumulated_arrays_keep_their_lower_bounds)

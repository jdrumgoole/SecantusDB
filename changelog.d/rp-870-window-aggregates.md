### Rust PG server: every built-in aggregate runs over a window

PostgreSQL runs any plain aggregate as a window function: `stddev(v) OVER
(ORDER BY v)` is a running standard deviation, `corr(y, x) OVER (...)` a
running correlation. This server ran only nine of them that way (`sum`,
`count`, `avg`, `min`, `max`, `string_agg`, `array_agg`, `bool_and`,
`bool_or`) and refused the rest. Now each of the others is planned as the
same call without OVER is, with the same argument-type checks, and computed
over each row's frame by the same aggregate code that computes it over a
group. So a window answer and a GROUP BY answer cannot drift apart.

#### Fixed

- `stddev*`, `var*` / `variance`, `bit_and` / `bit_or` / `bit_xor`,
  `every`, `json[b]_agg`, `json[b]_object_agg`, `corr`, `covar_*`, the
  `regr_*` family, `range_agg`, `range_intersect_agg` and `xmlagg` run
  under OVER, with FILTER, PARTITION BY, frames and named windows. The
  two-argument aggregates read both arguments per row. An empty frame
  returns what the aggregate returns over no rows (`regr_count` is 0).
- An aggregate's type error under OVER is PostgreSQL's 42883
  (`bit_and(double precision)`), as without OVER.
- `agg(x ORDER BY y) OVER (...)` is 0A000 `aggregate ORDER BY is not
  implemented for window functions`, as in PostgreSQL. Before, the inner
  ORDER BY was silently ignored.

Fixes RP-870 (tests/backlog/test_rp_found_rp682.py::test_more_aggregates_run_over_a_window); files RP-904.

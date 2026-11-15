### An ordered-set aggregate under OVER is refused as PostgreSQL refuses it

`select rank(1) over ()` resolves, as in PostgreSQL, to the hypothetical-set
aggregate `rank`, so it now fails 42809 `WITHIN GROUP is required for
ordered-set aggregate rank` instead of answering 1 as the window function. With
WITHIN GROUP, an ordered-set or hypothetical-set aggregate under OVER
(`percentile_cont(0.5) within group (order by v) over ()`) is 0A000 `OVER is
not supported for ordered-set aggregate percentile_cont`, PostgreSQL's message,
in place of a generic "not supported yet".

#### Fixed

- Ordered-set and hypothetical-set aggregates under OVER get PostgreSQL 15's
  42809 / 0A000 refusals, at the call.
  Fixes RP-869 (tests/backlog/test_rp_found_rp682.py::test_an_ordered_set_aggregate_over_a_window_is_refused)

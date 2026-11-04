### Rust PG server: EXPLAIN stacks a WindowAgg per distinct window

A SELECT with several distinct windows now explains as PostgreSQL 15 plans
it: one WindowAgg per window, stacked in `select_active_windows`' order,
each over a Sort only where the rows below are not already in its order,
and each priced by `cost_sort` and `cost_windowagg`.

#### Fixed

- `explain select a, row_number() over (order by a), rank() over (order by
  b) from e` printed a single WindowAgg with zero costs; it now prints the
  two WindowAggs and two Sorts PostgreSQL 15.19 prints, with its costs.
  Functions over one window share its WindowAgg, and a window whose keys
  prefix the order below needs no Sort of its own. A stack PostgreSQL
  would finish with an Incremental Sort still prints one WindowAgg.

Fixes RP-355 (tests/backlog/test_rp_explain_costs.py::test_plan[two windows stacked])

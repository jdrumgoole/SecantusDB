### Grouped sums, averages and other aggregates above an InitPlan's scan are priced

On the Rust PostgreSQL server, `EXPLAIN` of a `GROUP BY` whose WHERE
compares a column with an uncorrelated scalar subquery now prices the
`HashAggregate` over the scan for `sum`, `avg`, `min`, `max`, `bool_and`
and `bool_or` of stored columns, as it already did for counts. Before, any
aggregate but a count left the node printed with zero cost, rows and width.

#### Fixed

- Aggregates of stored columns grouped by stored columns over an
  InitPlan's scan, on a table with no index, are priced by `cost_agg`
  (AGG_HASHED) with `get_agg_clause_costs`' sums: one transition call an
  input row for each distinct call (identical calls share one state, as
  `find_compatible_agg` shares it), an `avg`'s final function once a group,
  and each result column its type's width. A repeated grouped `count(*)`
  no longer pays for two transition calls.
  Trims RP-440 (tests/backlog/test_rp_explain_costs.py::test_plan[a grouped sum above an initplan's scan is priced]).

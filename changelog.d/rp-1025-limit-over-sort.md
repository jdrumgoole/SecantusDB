### A LIMIT over an ORDER BY of one table is priced

`EXPLAIN` of a query that sorts one table's rows and keeps the first few
-- `select a from k where a > 5 order by b desc limit 3` -- now prints
PostgreSQL's costs: a top-N heap sort bounded by the LIMIT (and its
OFFSET), and the Limit's share of it. A sort key the select list does not
return is read by the scan as an extra column, widening its rows as it
does in PostgreSQL, so the same query without the LIMIT is priced too.

#### Fixed

- A Limit straight over a Sort of one relation's scan was printed with
  zero costs; it is now `cost_sort` with `limit_tuples` and
  `adjust_limit_rows_costs`, as on PostgreSQL 15.19. Where an index
  returns the ORDER BY's order the plan stays unpriced (the fractional
  path choice is not modelled).
- An ORDER BY on a stored column outside the select list declined the
  Sort's pricing; the column is now a resjunk entry of the scan.
- Fixes RP-1025 (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a limit over a descending sort])

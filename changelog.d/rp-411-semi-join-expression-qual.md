### An IN beside an expression's comparison is planned as a join

A WHERE that ANDs an `IN` / `EXISTS` subquery with a comparison of an
expression over the outer table, such as `length(b) > 1`, now prints
PostgreSQL's join in EXPLAIN, the outer scan priced with the expression's
cost per row. A scan's Filter now lists its quals in PostgreSQL's
`order_qual_clauses` order when one is a call of a built-in, the call
costing its `procost`, so a cheaper comparison prints first.

#### Fixed

- EXPLAIN of a sublink ANDed with an outer expression comparison printed an
  unpriced scan. Part of RP-411
  (tests/backlog/test_rp_explain_costs.py::test_plan[a sublink anded with an outer expression qual is planned as a join]).
- A scan's Filter with a built-in call printed its quals as written
  (tests/backlog/test_rp_explain_costs.py::test_plan[a scan's filter puts a cheaper qual before a call]).

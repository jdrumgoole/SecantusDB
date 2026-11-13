### EXPLAIN prices a join of three or more tables whose `<>` casts its operands

A join of three or more tables linked by inequalities now gets PostgreSQL's
estimate when one of its `<>` clauses casts or coerces an operand
(`e2.a::numeric <> e3.a::numeric`, `e2.a <> e3.a::numeric`,
`e2.a::bigint <> e3.a`). Before, it printed zeros. A cast operand is an
expression with no statistics, so its distinct count is PostgreSQL's
default of 200, as it already was for a two-table join.

#### Fixed

- EXPLAIN of a nested-loop join of three to eight tables whose `<>` join clause casts or coerces an operand prints PostgreSQL 15.19's costs and row estimate instead of zeros (RP-354, part).

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations with a coerced <> above])

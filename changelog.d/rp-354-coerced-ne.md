### EXPLAIN estimates a join whose `<>` coerces an operand

A nested loop joined by a `<>` (or IS DISTINCT FROM) between operands no single operator takes as they are -- `e left join k on e.a < k.a and e.a <> k.a::numeric`, or an `int` column against a `double precision` one -- now prints PostgreSQL's plan and estimate. PostgreSQL resolves such an operator by coercing an operand to the operator's input type, so `e.a <> k.a::numeric` compares `(e.a)::numeric <> (a)::numeric`: the coerced operand becomes an expression with no statistics, and its cast's function costs one call a row. PostgreSQL 15.19 prints `Nested Loop Left Join (cost=0.28..11975.00 rows=331667 width=14)` over `Index Scan using k_pkey on k (cost=0.28..8.64 rows=332 width=7)`. Before, such a join printed zeros. A coercion that is binary-coercible still prints zeros.

#### Fixed

- EXPLAIN of a nested loop whose `<>` or IS DISTINCT FROM join clause needs an operand coerced to resolve its operator prints PostgreSQL's costs and row estimate instead of zeros (RP-354, part).

#### Added

- Backlog item RP-918 (a LEFT join whose ON restricts its nullable side beside an inequality into its index prints no estimate), with a gap test.

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a coerced inequality into an index], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop over an integer and a double precision column])

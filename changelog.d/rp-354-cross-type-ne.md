### EXPLAIN estimates a join whose `<>` compares two integer types

A nested loop joined by a `<>` between operands of two types -- `e left join k on e.a < k.a and e.a <> k.a::bigint`, or two integer columns declared `int` and `bigint` -- now prints PostgreSQL's plan and estimate. PostgreSQL compares such operands through its cross-type operator (`int48ne`, `int24ne` and the like) without coercing either side, so the clause is estimated as one of a single type: a cast operand is an expression with no statistics, and two integer columns are matched by value through their most common values. PostgreSQL 15.19 prints the first `Nested Loop Left Join (cost=0.28..11142.50 rows=333000 width=14)` over `Index Scan using k_pkey on k (cost=0.28..7.81 rows=332 width=7)`; before, such a join printed zeros. A `<>` that PostgreSQL resolves by coercing an operand (`int <> numeric`) still prints zeros.

#### Fixed

- EXPLAIN of a nested loop whose `<>` join clause compares a cast operand of another integer type, or two integer columns of different types, prints PostgreSQL's costs and row estimate instead of zeros (RP-354, part).

#### Added

- Backlog items RP-916 (a parameterized inner index scan is never cached by a Memoize) and RP-917 (a `real` against `double precision` comparison prints a cast PostgreSQL does not make), each with a gap test.

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a cross-type inequality into an index], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop over integer columns of two types], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop over common values of two integer types])

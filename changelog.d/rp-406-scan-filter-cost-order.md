### A scan's Filter lists an expression's conjunct after a cheaper one

PostgreSQL orders a scan's quals cheapest first, pricing each by the operators it evaluates: an expression such as `a + 1 > 996`, a NOT BETWEEN's OR, or a column coerced by a call (`(a)::numeric`, an integer's cast to text) comes after a plain comparison. The Rust PostgreSQL server now prices those clauses the same way, alone or as a join's scan, where before it printed them in the order written.

#### Fixed

- EXPLAIN orders a scan Filter's expression, OR and coerced-column conjuncts by `cost_qual_eval`'s operator cost.
  Fixes RP-406 (tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's conjuncts in cost order], tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's or after a cheaper conjunct])

### EXPLAIN estimates a pattern match over an expression or a table never analysed

PostgreSQL 15 estimates `lower(b) like 'v%'` with `patternsel` even though the
expression has no statistics, and does the same for a LIKE or regex match on a
table that was never analysed. The Rust PostgreSQL server printed zero costs
for both. It now prints PostgreSQL's estimates: `rows=5` and
`cost=0.00..20.00` for the expression, which charges the call and the match,
and `rows=6` for `c like 'x%'` over a default-sized table.

#### Fixed

- EXPLAIN estimates a LIKE / ILIKE / regex match of a built-in call or
  arithmetic over columns (`ExprQual::pattern`), and a pattern match over a
  table never analysed (`where_selectivity`).
- Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a LIKE over an expression], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a LIKE over a table never analysed], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match over a table never analysed])

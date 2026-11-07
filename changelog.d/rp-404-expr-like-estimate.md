### Rust PG server: EXPLAIN estimates a scan filtered by an expression or a LIKE

`EXPLAIN` of one table's scan whose WHERE compares an expression with a
constant (`where a + 1 > 996`), or matches a text column with LIKE or
ILIKE (`where c like 'x%'`), now prints PostgreSQL 15.19's cost and row
estimate. Before, it printed `cost=0.00..0.00 rows=0 width=0`. A range
over a text column (`where c > 'w0500'`) in a bytewise collation is now
estimated from its histogram too, where before it printed zeros.

#### Fixed

- An expression with no statistics is estimated as `examine_variable`
  leaves it: a bound is `DEFAULT_INEQ_SEL`, two bounds on one expression
  are `DEFAULT_RANGE_INEQ_SEL`, and an equality is the reciprocal of
  `get_variable_numdistinct`'s guess. Each of the expression's operators
  is costed per row. This covers integer arithmetic and `||` over text.
  Function calls and casts are not covered yet.
- A LIKE or ILIKE is estimated by a port of `patternsel`. It uses the
  pattern's fixed prefix (`like_fixed_prefix`) and the histogram entries
  the pattern matches. For a histogram under 100 entries this is blended
  with the prefix's range: `prefix_selectivity`, `make_greater_string`
  and `pg_utf8_increment`. The rest of the pattern is guessed by
  `like_selectivity`, and the matching MCVs are added. A NOT LIKE is the
  complement less the NULLs. A position inside a text histogram bin is
  `convert_string_to_scalar`'s.
- Fixes RP-404
  (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a scan filtered by an expression],
  tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a scan filtered by a LIKE prefix]),
  with eighteen more regression scenarios beside them in that file.
- Filed RP-423: still no estimate for a function call compared with a
  constant, a LIKE over an expression or over a table never analysed, a
  regex match, or a join over any of these scans.

### Rust PG server: EXPLAIN prices an array column's `@>`, `<@` and `&&`

`EXPLAIN` of a query filtering an array column with `@>`, `<@` or `&&`
against a constant printed `cost=0.00..0.00 rows=0 width=0` and the filter
as `(...)`. Over a table never analysed it now prints PostgreSQL 15.19's
plan: `arraycontsel`'s default selectivity, one operator a row, and the
filter with the constant as its array type (`(a @> '{1}'::integer[])`).

#### Fixed

- A sequential scan whose WHERE is only array operators against constants
  is priced, and a constant on the left is commuted as `arraycontsel`
  does (`'{1}' <@ a`).
- The printed WHERE replaces the `Filter: (...)` placeholder, which a
  residual such as `1 = ANY (a)` had printed as a second Filter line.
- Fixes RP-367 (tests/backlog/test_rp_explain_costs.py::test_plan[an array containment]).

`1 = ANY (a)` and the operators over an analysed table still print zeros;
they are filed as RP-368.

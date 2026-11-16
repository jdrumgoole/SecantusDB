### A scan filtered by a built-in call is estimated

A WHERE that compares a built-in function call with a constant now gets
PostgreSQL's estimate: `select * from e where abs(a) > 5` over 1,000 analysed
rows is `Seq Scan on e (cost=0.00..20.00 rows=333 width=7)`, the comparison's
default selectivity and the call's `procost` on every row, as PostgreSQL
15.19 prints it, where it used to print zeros.

#### Fixed

- The Rust PostgreSQL server's single-relation scan estimate takes a call of
  a built-in whose every overload has one `procost`, over columns and
  literals, its result type chosen by the overload its arguments select.
  Partly fixes RP-423 (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a function call compared with a constant], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a call of another result type compared with a constant]); files RP-1021 (a call over constants is not folded in EXPLAIN).

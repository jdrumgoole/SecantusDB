### A function call in a nested loop's join filter is priced

A nested loop's Join Filter that calls a built-in function now charges the
call's `procost` on every pair it checks, as `cost_qual_eval` does: `select
* from e left join e e2 on e.a < abs(e2.a)` is PostgreSQL 15.19's `Nested
Loop Left Join (cost=0.00..17532.50 rows=333333 width=14)`, where it used
to be priced as the comparison alone.

#### Fixed

- The Rust PostgreSQL server's join-filter cost adds each column-reading
  call's `procost` (a call over constants is folded and free); a call whose
  `procost` depends on its overload leaves the join unpriced rather than
  guessed.
  Fixes RP-910 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a function call in its join filter])

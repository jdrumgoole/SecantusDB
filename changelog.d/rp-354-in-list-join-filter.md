### EXPLAIN estimates a LEFT join whose ON holds an IN list over the preserved side

A LEFT join's ON can test the preserved side alone against a list of
constants, as in `e left join e e2 on e.a <> e2.a and e.a in (1, 2, 3)`.
PostgreSQL folds the list into one array constant and keeps the conjunct
in the join's filter. It charges the comparison for half the list on
every pair the filter checks, on the assumption that the answer is settled
halfway through. It estimates the selectivity from the column's statistics
with `scalararraysel`. Before this change the Rust PostgreSQL server
printed zeros for the whole plan. It also printed the list as an `ARRAY[1,
2, 3]` constructor wherever a join held it, a scan's filter under a join
included.

The server now prices and estimates an IN or NOT IN list of fewer than
nine constants in that position the way PostgreSQL does. EXPLAIN's join
clauses print their constants folded, as PostgreSQL's planned expressions
are, so the list reads `= ANY ('{1,2,3}'::integer[])` under a join as it
does on a lone scan.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose ON holds an IN or NOT IN list
  of fewer than nine constants over a preserved-side column gives
  PostgreSQL 15.19's costs and rows. A list of one is the plain operator,
  and a list inside an OR is priced too.
- `EXPLAIN` prints an IN list under a join, in a Join Filter or in a scan's
  Filter, as one array constant, not an `ARRAY[...]` constructor.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's on conjunct over the preserved side is an in list],
  plus the NOT IN, one-element and OR variants beside it). The item stays
  open for a hashed list of nine or more, a regex match, a cast of a
  column, a call bounding an index, and joins of three tables into an
  index or under a coerced `<>`.
- Fixes RP-913
  (tests/backlog/test_rp_explain_costs.py::test_plan[an in list in a scan filter under a join is an array constant]).

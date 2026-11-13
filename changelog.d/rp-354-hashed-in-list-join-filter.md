### EXPLAIN estimates a LEFT join whose ON holds a long IN list over the preserved side

PostgreSQL treats an IN list of nine or more constants differently from a
short one. It builds a hash table of the list once, when the plan starts,
and then charges one hash and one comparison on each row it tests, rather
than a comparison for half the list. A LEFT join such as `e left join e e2
on e.a <> e2.a and e.a in (1, 2, 3, 4, 5, 6, 7, 8, 9)` keeps that test in
the join's filter, so the join's startup cost includes the hash table's
build: one hash per element, NULLs counted. Before this change the Rust
PostgreSQL server printed zeros for the whole plan once the list reached
nine.

The server now prices a list that long as PostgreSQL does, IN or NOT IN,
over integers or text. The startup lands on the Nested Loop whichever inner
it runs over: a Material of the nullable side, or an index scan of it
parameterized by the outer row.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose ON holds an IN or NOT IN list
  of nine or more constants over a preserved-side column gives PostgreSQL
  15.19's costs and rows, the hash table's build in the join's startup.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's on conjunct over the preserved side is a hashed in list],
  plus the NOT IN, NULL-element, text-list and index-scan variants beside
  it). The item stays open for a regex match, a cast of a column, a call
  bounding an index, and joins of three tables into an index or under a
  coerced `<>`. A hashed list inside an `OR` stays unestimated with the
  lone scan's (RP-028).

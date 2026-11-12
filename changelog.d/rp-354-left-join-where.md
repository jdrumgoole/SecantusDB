### EXPLAIN estimates a LEFT join filtered by a WHERE on its preserved side

A WHERE that names only the preserved side of a LEFT join, as in `e left
join e e2 on e.a <> e2.a where e.a > 990`, filters that side's rows before
the join, so PostgreSQL pushes it into that side's scan as its Filter. The
Rust PostgreSQL server already placed the Filter there, but its EXPLAIN
refused to estimate any LEFT join that had a WHERE, and printed zeros. It
now prints PostgreSQL 15.19's costs and row counts for these Nested Loops,
whether the WHERE is one condition or several, ANDed or ORed. A WHERE on
the nullable side that rejects NULLs already turns the join into an inner
one, as before, and any other WHERE still runs above the join, which is
not estimated.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose WHERE reads only the
  preserved side gives PostgreSQL's estimate. The WHERE is that side's
  scan Filter, and the join never estimates fewer rows than the filtered
  outer side.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a where],
  plus the two-conjunct, OR, nullable-side ON conjunct, inequality-join
  and indexed-inner variants beside it). The item stays open for a
  preserved side whose WHERE could be its Index Cond, a parameterized
  inner index scan under an ON restriction, and the other nested loops it
  lists.
- Filed RP-908: a LEFT join whose WHERE keeps only the unmatched rows
  (`where e2.a is null`) prints a Left Join with a Filter, where PostgreSQL
  plans an Anti Join.

### EXPLAIN estimates a LEFT join whose ON holds a LIKE over the preserved side

A LEFT join's ON can test the preserved side alone with a pattern, as in
`e left join e e2 on e.a <> e2.a and e.b like 'v1%'`. PostgreSQL keeps
that conjunct in the join's filter. It charges one operator for it on
every pair the filter checks, and estimates its selectivity with
`patternsel` from the column's statistics, as it does for a scan's
`Filter`. Before this change the Rust PostgreSQL server printed zeros for
the whole plan.

The server now prices and estimates a LIKE or ILIKE, and their negations,
there the way PostgreSQL does. That covers a pattern inside an OR, and a
join whose inner side is read through its index.

#### Fixed

- `EXPLAIN` of a Nested Loop Left Join whose ON holds a LIKE, ILIKE, NOT
  LIKE or NOT ILIKE of a preserved-side string column against a constant
  pattern gives PostgreSQL 15.19's costs and rows.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a pattern over its preserved side],
  plus the negated-ILIKE, indexed-inner and OR variants beside it). The
  item stays open for a cross-type `<>` into an index and a join of three
  relations. RP-915 filed: a LIKE with an ESCAPE clause prints a
  `like_escape` call and is not estimated.

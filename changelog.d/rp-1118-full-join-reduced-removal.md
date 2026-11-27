### Rust PG server: a FULL JOIN reduced to a LEFT JOIN nothing reads is removed

`EXPLAIN` of a FULL JOIN that a WHERE, or a join above it, reduces to a
LEFT JOIN onto a unique key nothing reads now plans its outer side alone,
as PostgreSQL 15 does. PostgreSQL's `reduce_outer_joins` runs before
`remove_useless_joins`.

#### Fixed

- `explain (costs off) select e.b from e full join k on e.a = k.a where
  e.a > 0`, with `k.a` the primary key, printed a `Hash Left Join` of `e`
  over `k`; it is now `Seq Scan on e` with `Filter: (a > 0)`. The same
  applies when an inner join above the FULL JOIN reduces it (`... join e e2
  on e2.a = e.a`, a Hash Join of `e` and `e2` alone). Join removal now
  judges each join by the kind `reduce_outer_joins_pass2` leaves it: what
  the WHERE proves non-NULL, and what each join above passes down from its
  own ON.
- Fixes RP-1118 (`tests/backlog/test_rp_explain_costs.py::test_plan[a full join a where reduces to a removable left join is removed]`,
  `[a full join an inner join reduces to a removable left join is removed]`).
  Files RP-1166 (a RIGHT JOIN, written or reduced from a FULL one, onto a
  key nothing reads is not removed), RP-1167 (a USING join's EXPLAIN
  prints no Hash Cond, and a FULL USING join no filter on its merged
  column) and RP-1168 (a FULL USING join reduced to a LEFT JOIN nothing
  reads is not removed).

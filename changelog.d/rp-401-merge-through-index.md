### A join on an indexed key merges through its index

A join of a small table to a large one on the large one's primary key now
explains as PostgreSQL plans it: a Merge Join (a Merge Right Join for a LEFT
join) reading the key's index in order and sorting only the small side,
priced to the cent. The merge's cost now follows PostgreSQL's
`mergejoinscansel`: a merge stops reading an input once it passes the other
side's largest key, so a 1,000-row index read to its thirtieth key costs a
few hundredths of its full scan, and the merge beats the hash join
PostgreSQL also weighs.

#### Fixed

- Rust PG server: `explain` of `s join k on s.a = k.a` and `s left join k
  on s.a = k.a` (`k.a` the primary key, `s` the smaller table), filtered on
  `k` or not, prints PostgreSQL 15.19's Merge Join over `Index Scan using
  k_pkey` and a Sort, with its costs, where it printed a Hash Join; the
  general merge paths apply `initial_cost_mergejoin`'s scan fractions.
  Fixes RP-401 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join on a primary key merges through its index]).

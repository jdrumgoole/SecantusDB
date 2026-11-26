### Rust PG server: a chain with a LEFT JOIN is drawn in the join order search's plan

Over tables never analysed, EXPLAIN now draws a chain of three tables
holding a LEFT JOIN in the plan the join order search picks, as
PostgreSQL plans it. The LEFT JOIN can sit below or above an inner join.
It can also be a FULL JOIN that an inner join above reduces to a LEFT
one (`reduce_outer_joins`). Each LEFT join is drawn as the search plans
it: a `Hash Left Join` with its preserved side outer, or a `Hash Right
Join` hashing that side where that is cheaper. Before, every such chain
was drawn as written, always hashing the nullable side.

#### Fixed

- `explain (costs off) select e.b, e2.b from e full join k on e.a = k.a
  join e e2 on e2.a = k.a` (never analysed) now matches a private
  PostgreSQL 15.20 cluster: a Hash Join of `e e2` over a Hash of the
  `Hash Right Join` that hashes `k`. So does the same join written as
  `k left join e ... join e e2 ...`.
- In a sweep of 144 three-table chains of 20, 300 and 3,000 rows (INNER,
  LEFT, RIGHT and FULL pairs), 116 plans now match, against 62 before.
  None that matched before was lost, and every query's rows matched both
  before and after.
- Over analysed tables the chain is still drawn as written: the search
  does not run there until RP-1059 (1). RP-1064 stays open for that case.
  Its gap test no longer expects an `Inner Unique` line, which
  PostgreSQL prints only under VERBOSE.
- Filed RP-1118: a FULL JOIN reduced to a LEFT JOIN to a unique key that
  nothing reads is not removed, as PostgreSQL's `remove_useless_joins`
  removes it.

Fixes RP-1064, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a full join an inner join reduces over never-analysed tables hashes its preserved side], tests/backlog/test_rp_explain_costs.py::test_plan[a left join below an inner join over never-analysed tables hashes its preserved side])

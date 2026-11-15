### A filter inside a join nested in an outer join runs in its table's scan

On the Rust PostgreSQL server, a condition that reads one table of an inner
join nested inside a LEFT or RIGHT join now runs in that table's scan, as
PostgreSQL's `distribute_qual_to_rels` places it: both the inner join's own
one-table ON conjuncts and the outer join's ON conjuncts over one table of
the inner join it null-extends. EXPLAIN prints them as that scan's
`Filter`, not as a `Join Filter`. The rows returned are unchanged.

#### Fixed

- An inner join under an outer join runs its one-table ON conjuncts in
  their tables' scans (RP-393).
- A LEFT or RIGHT join's ON conjunct over one table of an inner join on its
  nullable side runs in that table's scan (RP-397). Not through a lower
  outer join's nullable side, where PostgreSQL keeps it at the join.

The plan around them, its join order and method and its estimates, is filed
as RP-978.

Fixes RP-393 (tests/backlog/test_rp_explain_costs.py::test_plan[an inner join's one-table ON conjunct under an outer join]).
Fixes RP-397 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's ON conjunct over the hashed table of an inner join]).

### Rust PG server: a join's scan filtered by an expression prints its one real Filter

EXPLAIN of a join whose scan is filtered by an expression no longer prints
a `Filter: (...)` placeholder above the real line. A LEFT join that a WHERE
over an expression of its nullable side reduces to an inner join now prints
that conjunct on the side's scan, as PostgreSQL 15.19 does.

#### Fixed

- `explain (costs off) select * from e left join s on s.a = e.a where e.a + 1 > 996`
  prints `Filter: ((a + 1) > 996)` under `Seq Scan on e`, and nothing
  else there. Before, a `Filter: (...)` line came first. The same held
  under an inner join, for an ON conjunct over one table, and for a LEFT
  join's ON conjunct over its nullable side.
- `explain (costs off) select * from e left join s on s.a = e.a where s.a * 2 > 4`
  prints the reduced `Hash Join` with `Filter: ((a * 2) > 4)` on
  `Seq Scan on s`. Before, the plan reduced the join but EXPLAIN kept the
  conjunct as the join's `Join Filter` and left `Filter: (...)` on the
  scan. EXPLAIN now proves a side non-NULL with the same test the planner
  uses, which looks through casts and arithmetic.

Fixes RP-405 (tests/backlog/test_rp_explain_costs.py::test_plan[a join's scan filtered by an expression], tests/backlog/test_rp_explain_costs.py::test_plan[an inner join's scan filtered by an expression], tests/backlog/test_rp_explain_costs.py::test_plan[a left join reduced by a where over an expression])

### EXPLAIN removes a LEFT JOIN to a view, a CTE or a subquery nothing reads

The Rust PostgreSQL server's EXPLAIN removed a LEFT JOIN nothing above it
reads only when its inner side was a plain table. PostgreSQL first pulls a
view, a CTE it inlines and a simple FROM subquery up into the query, so a
join to one of those over a single table is removed just as a join to the
table is, when that table is unique on the columns the ON fixes. A subquery
with no FROM is a single row, and PostgreSQL removes a LEFT JOIN to it
whatever the ON says. `explain select e.b from e left join kv on kv.a = e.a`
(with `kv` a view over `k`) and `with k as (select 1 as a) select e.b from e
left join k on k.a = e.a` now both print `Seq Scan on e`, as PostgreSQL 15
does.

#### Fixed

- `secantus-pgplan` (`join_removal.rs`): the inner side of a LEFT JOIN is
  taken as `pull_up_subqueries` leaves it. A view, a CTE that `inline_cte`
  inlines (not MATERIALIZED, read once or NOT MATERIALIZED, no volatile
  call) and a non-LATERAL FROM subquery that passes `is_simple_subquery`
  resolve, through any chain of them, to one table. Each column they show
  is mapped to that table's column, column alias lists included, and an
  equality to a constant in their WHERE fixes its column as a
  `baserestrictinfo` does. A security barrier view, a subquery with ORDER
  BY, LIMIT, DISTINCT, GROUP BY, an aggregate, a window function, a
  set-returning or a volatile call, and a join inside the subquery are
  each left in place, as PostgreSQL leaves them.
- A FROM-less subquery with no WHERE is removed with no index asked
  (`remove_useless_result_rtes`), before any other removal.
- `secantus-pg` publishes the views marked `security_barrier` to the
  planner.

Fixes RP-1106 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a left join to a view nothing reads is removed], tests/backlog/test_rp_explain_costs.py::test_plan[a left join to a one-row cte nothing reads is removed]). Left open: a subquery distinct on its ON, a NATURAL join, and a one-row join under ON TRUE whose column is read. Filed RP-1155, RP-1156, RP-1157 and RP-1158.

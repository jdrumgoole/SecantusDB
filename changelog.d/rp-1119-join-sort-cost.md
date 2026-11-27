### EXPLAIN prices the Sort of an ORDER BY over joined rows and over a join a WHERE fixes

`secantusd-pg` priced the Sort of a two-table join's ORDER BY only on its
narrow join path. EXPLAIN now prices it in two more cases, as PostgreSQL 15
prices it: when the planner keeps the ORDER BY above joined rows (an output
alias, an ordinal, a comma join), and when a WHERE constant fixes a join
key. Before, every node of those plans was `cost=0.00..0.00 rows=0 width=0`.

#### Fixed

- `explain select e.b as x, k.b from e join k on e.a = k.a order by x`,
  `... order by 1` and `from e, k where e.a = k.a order by k.b` price the
  Sort as `cost_sort` over the join's cheapest path. The join's width counts
  an ORDER BY column the select list does not take. `EXPLAIN ANALYZE` prints
  the join's own counts under the Sort.
- `explain select e.b, k.b from e join k on e.a = k.a where k.a = 5 order by
  e.b` printed an unpriced Hash Join. It now prints PostgreSQL's priced Sort
  over the Nested Loop into `k_pkey` that the constant's equivalence class
  leaves.
- An ORDER BY on a column a WHERE equality fixes (`where e.b = 'v1' order by
  e.b`) is no longer priced as a Sort, because PostgreSQL sorts nothing
  there. It is left unpriced until that plan is drawn (RP-1172).
- Fixes part of RP-1119 (`tests/backlog/test_rp_explain_costs.py::test_plan[an order by an output alias over a join prices its sort]`,
  `test_plan[an order by over a join a where fixes prices its sort]` and six
  more). What remains open is an ORDER BY on a join key, which needs the
  merge paths' order weighed. Files RP-1172 (no Sort for a column a WHERE
  constant fixes) and RP-1173 (a joined-rows Hash prints its memory 1kB
  short).

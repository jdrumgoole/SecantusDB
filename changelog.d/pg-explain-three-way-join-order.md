### Rust PostgreSQL server: EXPLAIN draws a three-way join in the order the planner picks

`secantusd-pg` has run a chain of three joined tables in the plan
PostgreSQL's join order search picks since RP-028, so its rows come out
in PostgreSQL's order. EXPLAIN never drew that plan, though: it printed
the joins as written, `(x join y) join z`, every time. Now, for a chain
of inner hash joins, EXPLAIN draws the plan the search keeps: which pair
joins first, which side of each join is hashed, and each join's `Hash
Cond`. Over one equivalence class (`e.a = k.a ... e2.a = k.a`) that is
the clause PostgreSQL derives from the class's first member on each side.

The search itself now joins a pair to the third table on the class's
first member in that pair, as `generate_join_implied_equalities` does.
Before, it always used the first ON's left column, which priced the hash
buckets by the wrong column's statistics when the ON was written the
other way round (`on k.a = e.a`).

This covers tables the search plans today: those not yet analysed. A
chain over an analysed table, a merge join or nested loop in the plan,
and VERBOSE and ANALYZE still print the written order (RP-1059).

#### Fixed

- `secantus-pg` (`join_search.rs`, `lib.rs`): EXPLAIN of an inner
  three-way hash-join chain is drawn in the join order search's plan
  (`reorder_three_table_chain`). Over one equivalence class, the search
  joins each pair on the class's first member there (`class_members`).
- `secantus-pgplan` (`joincost.rs`): a chosen join path records whether
  it is a hash join (`JoinChoice::hash`). A hash join with `a` outer on a
  unique `b` and a nested loop shared `JoinMethod::Other`.
- Part of RP-1059 (tests/backlog/test_rp_explain_costs.py::test_plan[a
  three-way join never analysed is drawn in the searched order],
  tests/backlog/test_rp_explain_costs.py::test_plan[a three-way chain
  never analysed joins its second pair first],
  tests/backlog/test_rp_explain_costs.py::test_plan[a three-way join
  hashes a pair on its class's first member]); the item stays open for
  analysed tables.

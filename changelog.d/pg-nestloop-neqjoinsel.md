### EXPLAIN prices a nested loop with a `<>` join filter

On the Rust PostgreSQL server, `EXPLAIN` of a comma join of two unindexed,
analysed tables whose WHERE pairs an inequality with a `<>` between the two
tables (`from e, e e2 where e.a < e2.a and e.b <> e2.b`) now prints
PostgreSQL 15.19's `Nested Loop (cost=0.00..17532.50 rows=300000 width=14)`
instead of zeros.

#### Fixed

- `secantus-pg` (`scan_estimate.rs`): the nested-loop estimate takes a `<>`
  or `IS DISTINCT FROM` conjunct as `neqjoinsel`, one minus `eqjoinsel_inner`
  over the two columns' distinct counts, null fractions and MCV lists,
  multiplied into the clause list in order as `clauselist_selectivity` does.
  The column statistics lookup is shared with the hash-join estimate.

Fixes part of RP-354 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop with a not-equal filter]); RP-354 stays open for the ON, LEFT, indexed and three-way nested loops.

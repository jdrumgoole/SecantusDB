### Rust PG server: EXPLAIN VERBOSE prints the Output of a join nested in an outer join

`EXPLAIN VERBOSE` of a relation joined to a nested two-relation join,
`s left join (e join k on e.a > 998 and k.a = e.a) on true`, printed the
plain plan: no `Output` line on any node, and the quals unqualified. It now
prints what PostgreSQL 15.20 prints.

#### Fixed

- Every node of such a plan prints its target list. The top join prints the
  query's list. The nested join, and a Materialize or Hash over it, print
  the nested join relation's list: each relation's columns that the query
  or the outer ON still reads, in FROM order (`build_joinrel_tlist`). The
  nested join's own scans print their physical lists. The outer relation's
  scan prints its physical list, or its `reltarget` under a Hash or a
  Materialize (`CP_SMALL_TLIST`). Scans are named schema-qualified and their
  quals prefixed. This covers a LEFT or inner join over the nested pair,
  with an ON of `true` or (for a LEFT join) column equalities, drawn as a
  nested loop, a hash left join or a hash right join.
- A conjunct of an inner ON that reads one relation,
  `e join k on e.a > 998 and k.a = e.a`, is now that relation's
  restriction, as a WHERE conjunct is (`distribute_qual_to_rels`). Its
  scan's `Filter` prints prefixed under VERBOSE, so the two-relation join
  prints every node's `Output` too, instead of the plain plan (RP-966, in
  part).
- A two-relation nested loop's Index Only Scan of a one-column primary key
  printed the table's columns as its `Output` (`k.a, k.b`). It now prints
  the index's target list (`k.a`), as PostgreSQL does.

Fixes RP-1121 (tests/backlog/test_rp_explain_costs.py::test_plan[verbose of an inner join nested in a left join]),
with regressions in the same file (`verbose of a nested join returning only
the columns above it`, `verbose of a hash right join over a nested hash
join`, `verbose of an inner join whose on restricts one relation`) and
`secantus-pgplan`'s `ruleutils::verbose_nested_join_tests`.

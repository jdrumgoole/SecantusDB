### Rust PG server: EXPLAIN prices a merge join on an expression of each side, and one an ORDER BY its merge key needs no Sort for

`EXPLAIN` of a two-table join whose one ON equality has an expression on
each side (`e.b || 'y' = e2.b || 'x'`, `e.a + 1 = e2.a + 2`) printed an
unpriced Nested Loop with the equality as its Join Filter; it now prints
PostgreSQL 15's priced Merge Join, each input a Sort on its expression,
inner and LEFT alike. And an ORDER BY of the merge key over a join whose
cheapest path is that merge (`e join e e2 on e.b = e2.b || 'x' order by
e.b`) printed an unpriced Sort over an unpriced Nested Loop; it now prints
the Merge Join alone, as `create_ordered_paths` keeps a cheapest path
already in the query's order, its inputs sorted in the ORDER BY's
direction (`Sort Key: e.b DESC`). An ORDER BY of another column over such
a join is now the priced Sort over the Merge Join.

#### Fixed

- Fixes RP-1131 (tests/backlog/test_rp_explain_costs.py::test_plan[a join on an expression of each side is a merge join], tests/backlog/test_rp_explain_costs.py::test_plan[a join on an expression key ordered by its merge key is a merge join])
- Filed RP-1239 (a merge on several clauses, one an expression of each
  side), RP-1240 (an expression key beside an ON inequality) and RP-1241
  (a join on a cast of an expression).

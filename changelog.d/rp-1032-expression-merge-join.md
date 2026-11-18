### A join on an expression key the merge wins is a priced Merge Join

`select * from e join e e2 on e.b = e2.b || 'x'`, over a column of ten
values, now explains as PostgreSQL 15.19 does: a `Merge Join` at its costs,
each input under a `Sort`, the expression side's `Sort Key` the scan's
projected value `((e2.b || 'x'::text))` and the join's `Merge Cond: (e.b =
((e2.b || 'x'::text)))`, where it printed an unpriced `Nested Loop`. A merge
join's condition is also written outer side first, as `create_mergejoin_plan`
switches it, however the ON wrote it.

#### Fixed

- Rust PG server: an inner join on an expression key that the merge path
  wins is drawn and priced as PostgreSQL's Merge Join.
  Fixes RP-1032 in part (tests/backlog/test_rp_explain_costs.py::test_plan[an expression key over a column of few values is a merge join]); RP-1071 filed.

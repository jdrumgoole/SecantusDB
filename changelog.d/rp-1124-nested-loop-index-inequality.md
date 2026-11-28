### A nested loop over an indexed side a join inequality reads is priced

`EXPLAIN` of a join PostgreSQL runs as a nested loop, where a join `<` or
`>` reads a table whose own WHERE is an index qual -- `select e.b from e
join k on e.a < k.a where k.a = 5` -- now prints PostgreSQL's costs and
plan instead of zeros. The one-row `Index Only Scan using k_pkey on k` is
the loop's outer. Where the other table has an index too, its scan is
parameterized by that row (`Index Cond: (a < y.a)`). Where the
parameterized path wins, the table's own `=` joins the join's bound in
one Index Cond (`((a > w.a) AND (a = 5))`), under a Memoize.

#### Fixed

- A nested loop's indexed side that a join inequality could
  parameterize was refused whenever its own cheapest path was an index
  path, and whenever its own filter held an equality. Both paths are now
  weighed, as `add_path` keeps them. The side's own path is an index
  scan, an index-only scan, or a Materialize over one (`where k.a > 990`).
  The parameterized scan takes the side's `=` on the leading index column
  as an index qual beside the join's bound: one tuple read on a unique
  index (`btcostestimate`), `clauselist_selectivity` over both.
- Fixes RP-1124 (tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop over an index qual a join inequality also reads is priced], tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop's one-row outer parameterizes the other index])
- Filed RP-1244: an index equality beside a `<>` of another column is
  still unpriced.
- Filed RP-1245: a WHERE equality to a cast of the other side beside a
  join `<` is a Nested Loop where PostgreSQL hashes it.

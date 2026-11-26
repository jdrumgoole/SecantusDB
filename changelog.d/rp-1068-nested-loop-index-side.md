### A nested loop over a side's own index qual is priced

`EXPLAIN` of a join PostgreSQL runs as a nested loop, where one table's
own WHERE is an index qual -- `select e.b from e join k on e.a <> k.a
where k.a = 5` -- now prints PostgreSQL's costs and its plan instead of
zeros: the one-row `Index Only Scan using k_pkey on k` as the loop's
outer, the other table's Seq Scan inside it.

#### Fixed

- A nested loop's side over an indexed table was refused whenever its own
  filter was one an index could take. Where no join clause can
  parameterize that side (it is read only by `<>`, never an index qual),
  its only paths are its own restriction's, and the cheapest of them --
  an index scan, an index-only scan or its sequential scan -- is now the
  side the loop is priced over, either side of the loop, an inner join's
  or a LEFT join's nullable side.
- The EXPLAIN tree draws each side as the path the cost model chose, so a
  side the storage planner read through an index is renamed to the Index
  Only Scan or Seq Scan PostgreSQL prints.
- Fixes RP-1068 (tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop over an inner relation's index qual is priced])
- Filed RP-1124: a side whose own index qual a join `<` or `>` also reads
  is still unpriced, as is the other side's index parameterized by it.
- Filed RP-1125: an Aggregate over a Nested Loop is unpriced.

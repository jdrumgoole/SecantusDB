### An aggregate over a nested loop is priced in EXPLAIN

PostgreSQL plans a plain aggregate over a join by planning the join first
and putting AGG_PLAIN over its cheapest path, so `select count(*) from e
join k on e.a <> k.a` is the same Nested Loop the select-list form draws,
each scan narrowed to the columns the join and the aggregates read and the
inner materialised. The Rust PostgreSQL server priced that loop on its own
(RP-329), but printed zero costs on the Aggregate and the Nested Loop below
it. It now prints PostgreSQL 15's plan and costs, for an inner or a LEFT
loop, a cross join, and a loop whose inner side is filtered.

#### Fixed

- EXPLAIN of a plain aggregate (no GROUP BY, HAVING, ORDER BY, LIMIT or
  DISTINCT) over a two-table Nested Loop prices the loop as the select of
  the rows it aggregates (`cost_nestloop`, with its Materialize) and the
  Aggregate over it (`cost_agg`, AGG_PLAIN), as RP-1129 did for a hash
  join.
  Fixes RP-1125 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a nested loop is priced])

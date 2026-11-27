### EXPLAIN pulls up a one-row constant subquery in a SELECT's FROM

The Rust PostgreSQL server's EXPLAIN now plans a FROM-less subquery of
literal targets beside another FROM item as PostgreSQL's
`pull_up_subqueries` does: `select e.b, count(*) from e, (select 1) x
group by e.b` is a `HashAggregate` with `Group Key: e.b` over `Seq Scan
on e`, where it was a `Nested Loop` of `e` and `Subquery Scan on x` under
the internal joined-row key `Group Key: e\x1Fb`. A join to one on a column
becomes the scan's `Filter` with the constant in place, and the keys print
qualified, as the subquery's range table entry stays.

#### Fixed

- A cross or inner join of a table with a one-row constant subquery is the
  table's own scan in EXPLAIN, its key qualified and its constant inlined.

Fixes RP-1107 (tests/backlog/test_rp_explain_costs.py::test_plan[a group by over a cross join with a one-row subquery prints its key])

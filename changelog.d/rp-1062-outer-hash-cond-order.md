### EXPLAIN prints every clause of a multi-clause Hash Cond outer side first

The Rust PostgreSQL server's `EXPLAIN` now switches each clause of a hash or
merge join's condition so that the outer relation's operand comes first, as
PostgreSQL's `get_switched_clauses` does. Before this, it switched a
one-clause condition only.

#### Fixed

- `explain (costs off) select e.b, e2.b from e full join e e2 on e2.a = e.a
  and e2.b = e.b` printed `Hash Cond: ((e2.a = e.a) AND (e2.b = e.b))`.
  PostgreSQL 15.19 prints `Hash Cond: ((e.a = e2.a) AND (e.b = e2.b))`, and
  so does this server now. A nested loop's `Join Filter` is not switched,
  as in PostgreSQL.
- Fixes RP-1062 (`tests/backlog/test_rp_explain_costs.py::test_plan[an outer join's two-clause hash cond puts the outer column first]`;
  regression test `explain::tests::each_clause_of_a_hash_cond_is_written_outer_side_first`).

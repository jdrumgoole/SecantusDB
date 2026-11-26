### Rust PostgreSQL server: EXPLAIN prices a join on an expression key against a unique column, across integer types, and as a Merge Left Join

`EXPLAIN` now prices the join PostgreSQL 15 plans for an expression key
against a unique column, for a cast key against another integer type, and
for a LEFT join on an expression key where the merge wins. Each printed an
unpriced `Nested Loop` with a `Join Filter` before.

#### Fixed

- A join whose key is an expression against a unique column (`select *
  from e join k on k.a = e.a - 1`) now weighs PostgreSQL's merge, which
  reads the column's index presorted, and its nested loop, which probes
  that index by the expression. The index probe is charged the
  expression's `qual_arg_cost` on each scan. A Memoize over the probe is
  keyed on the column the expression reads. The plan is PostgreSQL's
  `Hash Join (cost=27.50..45.26 rows=1000 width=14)`, inner unique, and
  the same for LEFT and RIGHT joins and for a side a WHERE makes the
  smaller one.
- A cast join key is charged as PostgreSQL's `cost_qual_eval` charges it:
  its `pg_cast` function, or its I/O conversion. `e.a::bigint` is one
  operator (`int48`); before, a cast was charged nothing. A cast key
  against a key of another integer type now hashes, because `int84eq` and
  its kin belong to the one `integer_ops` family. `select * from e join e
  e2 on e.a::bigint = e2.a` is a `Hash Join (cost=27.50..57.50 rows=1000
  width=14)`.
- A LEFT join on an expression key where the merge wins is drawn as
  PostgreSQL's `Merge Left Join`, sorted on the expression's projected
  value.

Fixes RP-1032 (tests/backlog/test_rp_explain_costs.py::test_plan[an expression key against a unique column is a hash join]).
Filed RP-1127, RP-1128, RP-1129 and RP-1130 while working it.

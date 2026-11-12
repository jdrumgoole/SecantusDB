### EXPLAIN prices a cast inside a join clause

A join clause can cast a column, as in `e left join k on e.a < k.a and
e.a <> k.b::int`. PostgreSQL charges that cast on every row it checks. A
cast through a `pg_cast` function costs one call. A cast through the
types' text forms (`text` to `int`) costs two, the output and the input
function. PostgreSQL also has no statistics for the cast value, so it
estimates the `<>` from a default 200 distinct values. Before this change
the Rust PostgreSQL server printed zeros for such a `<>`. For a `<`, `<=`,
`>` or `>=` over a cast it printed a total that left the cast out.

The server now costs the cast by its `pg_cast` method and estimates the
clause the way PostgreSQL does. Where the inner side is read through its
index, the clause becomes that scan's `Filter`. That scan's row estimate
treats the other side's value as a constant (`var_eq_non_const`), so it is
332 where the join's own `neqjoinsel` estimate gives 333.

#### Fixed

- `EXPLAIN` of a Nested Loop whose join clause is a `<>` over a cast of
  either side's column gives PostgreSQL 15.19's estimate. With the inner
  read through its index, the clause is that Index Scan's `Filter`.
- `EXPLAIN` of a Nested Loop whose join filter casts a column (an
  inequality, or a comma join's WHERE) includes the cast's cost; it was
  left out before.
- A parameterized inner index scan's rows estimate each moved `<>` from
  the inner column's own statistics (`var_eq_non_const`), not from the
  join's `neqjoinsel`.
- Fixes RP-354 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join with a cast clause into an index],
  plus the inner-join, outer-cast, join-filter and inequality variants
  beside it). The item stays open for a one-cent rounding tie,
  binary-coercible and cross-type casts, and the other nested loops it
  lists. RP-910 filed: a function call in a join filter is still priced
  as nothing.

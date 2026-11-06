### Rust PG server: EXPLAIN of a WHERE that folds to false plans no scan

PostgreSQL simplifies the constants in a WHERE before it plans anything. A
strict comparison with a NULL is NULL, an AND with a false or NULL argument
is false, and an OR drops its false and NULL arms. When the whole WHERE comes
out false or NULL, the relation is dummy and the plan is a `Result` with
`One-Time Filter: false` and no scan under it. `explain select * from e
where a = null` printed `Index Scan using e_pkey on e`, naming an index the
table does not have (it has no primary key), with the comparison as its
Filter. It now prints PostgreSQL 15.19's plan.

#### Fixed

- EXPLAIN of a SELECT over tables, an UPDATE or a DELETE simplifies its
  WHERE as `eval_const_expressions` and `canonicalize_qual` do: strict
  comparisons over NULL fold, a BETWEEN with a NULL bound is taken apart into
  its comparisons, a NOT of a constant is negated, an `IS [NOT] NULL` of a
  NULL is a constant, and AND / OR are reduced over their constants. A WHERE
  that ends false or NULL prints a `Result` with `One-Time Filter: false`
  where the scan or join would be, under any Aggregate, Sort or ModifyTable
  above it, with `actual rows=0 loops=1` under ANALYZE. One that ends true
  prints no Filter, and an OR's NULL arm is gone from it (`a = null or b =
  'v3'` is `Filter: (b = 'v3'::text)`).
- EXPLAIN no longer names a `<table>_pkey` index for a table with no primary
  key.

Fixes RP-409 (tests/backlog/test_rp_explain_costs.py::test_plan[a comparison
with a null constant is a false one-time filter],
tests/backlog/test_rp_explain_costs.py::test_plan[a between with a null bound
is a false one-time filter]).

#### Found

RP-413 (a WHERE of a non-boolean type answers rows instead of 42804),
RP-414 (EXPLAIN skips operator type checks), RP-415 (no estimates above a
dummy relation), RP-416 (a COALESCE's constant arguments unfolded) and
RP-417 (a FROM-subquery is not pulled up), each with its gap test; and a
NULL-arm case added to RP-412.

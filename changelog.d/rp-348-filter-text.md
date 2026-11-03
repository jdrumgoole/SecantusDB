### Rust PG server: EXPLAIN prints a Seq Scan's Filter as PostgreSQL does

A Seq Scan's `Filter` line now matches PostgreSQL 15: an `IN` list of
constants prints as the array constant the planner folds it to, the
clauses come in the planner's order, and a range on an analysed text
column gets an estimate.

#### Fixed

- `where a in (1, 2, 3)` prints `(a = ANY ('{1,2,3}'::integer[]))`. The
  list is folded to one array constant as `eval_const_expressions` folds
  it, and its strings are quoted the way `array_out` quotes them.
- The clauses of a Filter come in PostgreSQL's order: a `column =
  constant` joins an equivalence class and comes back after the other
  clauses, and `order_qual_clauses` then sorts them by cost per row. So
  `where a = 5 and b > 'q'` prints `((b > 'q'::text) AND (a = 5))`.
- A range on an analysed text column whose values are all most common
  values is estimated from them when the collation compares bytes (`C`,
  `POSIX`, `C.UTF-8`). Before, it printed zeros.

Fixes RP-348 (tests/backlog/test_rp_explain_costs.py::test_plan[an IN list's filter]).

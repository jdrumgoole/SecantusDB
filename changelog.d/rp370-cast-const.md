### Rust PG server: EXPLAIN prints a cast constant folded (RP-370)

EXPLAIN now prints an explicit cast of an integer constant as the constant
PostgreSQL's `eval_const_expressions` folds it to, in `get_const_expr`'s
form: `where 1::bigint = any(a)` shows `Filter: ('1'::bigint = ANY (a))`,
as PostgreSQL 15.19 does, not `((1)::bigint = ANY (a))`. `pg_get_viewdef`
still prints the unfolded cast, as PostgreSQL does for a stored view.

#### Fixed

- Fixes RP-370 (tests/backlog/test_rp_explain_costs.py::test_plan[a cross-type constant equal to ANY of an array column])

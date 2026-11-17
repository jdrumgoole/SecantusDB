### A Sort or UPDATE over a WHERE that folds to false is priced as PostgreSQL prices it

When a WHERE folds to constant false, PostgreSQL 15 plans a `Result` with
`One-Time Filter: false` and still prices the nodes above it. The Rust PostgreSQL
server now does the same for a Sort above that Result: the select list is
sorted at its types' default widths, so `Sort (cost=0.01..0.02 rows=0 width=36)`.
An UPDATE's Result now reports the width of the assigned columns plus the row's
`ctid`. Sort costs also use PostgreSQL's own `LOG2` macro, so their last printed
digit matches.

#### Fixed

- EXPLAIN prices a Sort above a dummy relation, and gives an UPDATE's dummy
  Result its width (`dummy_target_width`). Every sort cost uses `costsize.c`'s
  `LOG2` (`costsize::pg_log2`).
- Fixes RP-420 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a sort over a dummy relation is priced], tests/backlog/test_rp_explain_costs.py::test_plan[an update of a dummy relation keeps its result's width])

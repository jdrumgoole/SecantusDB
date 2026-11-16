### A chain of inequality joins under an OFFSET runs in PostgreSQL's order

A chain of three or more tables joined only by inequalities, under a LIMIT
with an OFFSET, now runs in the join order PostgreSQL's search keeps, and
EXPLAIN prices it: the top path is the one the LIMIT reads least of past its
OFFSET, and the Limit's startup carries the OFFSET's share. It ran in the
written order, its plan printed with zero costs.

#### Fixed

- Rust PG server: `nest_loop_chain` accepts a constant OFFSET and passes it
  to `get_cheapest_fractional_path`'s choice.
  Fixes RP-919, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a chain of inequality joins under a limit and offset])

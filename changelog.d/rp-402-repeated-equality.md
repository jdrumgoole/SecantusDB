### A repeated WHERE equality is planned once

PostgreSQL's `process_equivalence` finds an equality that repeats an
earlier one redundant, so `where e.b = k.b and e.b = k.b` is one hash
clause and a repeated `e.a = 5` one scan filter, each priced once. The Rust
PostgreSQL server now drops such a repeat before it plans a join, so its
clauses, estimates and costs match.

#### Fixed

- A repeated join equality is one `Hash Cond`, estimated once
  (`rows=100000`, not `10000`), and a repeated `column = constant` one
  `Filter`.
  Fixes RP-402 (tests/backlog/test_rp_explain_costs.py::test_plan[comma join with a repeated join equality], tests/backlog/test_rp_explain_costs.py::test_plan[comma join with a repeated constant equality])

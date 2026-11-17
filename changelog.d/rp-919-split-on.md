### Each clause of a join chain's ON lands on its own join

A chain of inequality joins whose ON reads tables that the searched tree
puts on different joins now runs in that tree: each clause of the ON moves,
alone, to the lowest join holding the tables it reads, as PostgreSQL's
`distribute_qual_to_rels` places it, and clauses from different ONs that
land on one join are checked together. EXPLAIN prints and prices the tree
as PostgreSQL 15.19 does, each join's `Join Filter` listing the clauses
placed on it.

#### Fixed

- A chain whose last ON split across two joins of the searched tree ran in
  its written order and printed unpriced.
  Fixes RP-919 (tests/backlog/test_rp_explain_costs.py::test_plan[a bushy chain whose last ON splits across two joins], tests/backlog/test_rp_explain_costs.py::test_a_bushy_chain_whose_on_splits_runs_each_clause_where_it_lands)

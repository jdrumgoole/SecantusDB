### EXPLAIN prices a join whose select list holds a constant

A join whose select list computed a constant -- `select 'x' from k, e where
k.a = e.a`, or the `set b = 'x'` an `UPDATE ... FROM` makes into one -- printed
its plan unpriced. Each constant is now a column of the join's target list, of
its type's width and no cost to evaluate, as PostgreSQL's
`set_pathtarget_cost_width` sets it, so the plan prints PostgreSQL 15.19's
costs and widths on the comma-join and ON-join paths alike.

#### Fixed

- EXPLAIN of a two-table hash join whose select list holds constants prints
  PostgreSQL's estimate, its width counting each constant's type.
  Fixes RP-435 (tests/backlog/test_rp_dml_from.py::test_explain_join_width_of_a_computed_set_value)

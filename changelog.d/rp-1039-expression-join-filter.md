### A join filter comparing an expression with a column is priced

`EXPLAIN` of a hash join whose WHERE (or ON) compares an expression of one
table with a column of the other -- `where length(k.b) <> e.a` -- now
prints PostgreSQL's costs instead of zeros. The expression has no
statistics, so its `<>` is estimated as PostgreSQL does for one: the
default 200 distinct values against the column's own.

#### Fixed

- A `<>` between an expression over one relation's columns and the other
  relation was not estimated; it is now `neqjoinsel` over an operand with
  no statistics, of the expression's result type, as a cast key already
  was, and a hash join accepts it (and a cast key) as its join filter.
- Fixes RP-1039 (tests/backlog/test_rp_explain_costs.py::test_plan[a hash join with an expression join filter is priced])

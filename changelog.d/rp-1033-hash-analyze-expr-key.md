### Rust PG server: EXPLAIN ANALYZE counts below a hash join on an expression key, and below a hash anti join

`EXPLAIN ANALYZE` of a hash join whose key is an expression on one side
(`e.a = e2.a + 1`) now prints what PostgreSQL 15.19 prints below the
join: each scan's and the Hash's `actual rows`, and the Hash's `Buckets`,
`Batches` and `Memory Usage`. A `Hash Anti Join` (a LEFT join whose WHERE
keeps only its NULL-extended rows) gets the same counts, on a column key
or an expression key. Before, both printed only the join's own rows.

#### Fixed

- The recount evaluates the key expression on each row, as the Hash and
  the join evaluate their hash keys, rather than reading the bare column
  it is built on; an anti join counts each outer row no hashed row
  matches. Inner, LEFT (`Hash Right Join`), RIGHT (`Hash Left Join`) and
  anti joins all match PostgreSQL 15.19, with the expression on either
  side. A Merge Join on an expression key still prints no counts below
  it; RP-1033 stays open for that part.

Fixes RP-1033 in part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an anti join on an expression key]; also `[analyze of an anti join on a column key]`, `[analyze of an inner join on an expression key]`, `[analyze of an expression key join returning one column]`, `[analyze of a left join on an expression key]`)

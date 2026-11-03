### Rust PG server: EXPLAIN prints each set-operation arm's Filter

EXPLAIN of a UNION, INTERSECT or EXCEPT now prints each arm's WHERE on that
arm's scan, as PostgreSQL 15.19 does: `select a, b from e where a < 100
union all select a, b from k` shows `Filter: (a < 100)` under the Append's
`Seq Scan on e`, and an index-usable conjunct prints as `Index Cond`.

#### Fixed

- A set operation's scans printed no `Filter` or `Index Cond`; the WHERE
  was attached only to a statement over one relation.

Fixes RP-346 (tests/backlog/test_rp_explain_costs.py::test_plan[an append of a filtered text column])

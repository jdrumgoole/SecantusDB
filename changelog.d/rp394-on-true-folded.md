### Rust PG server: a constant `true` ON conjunct is folded away

A literal `true` in a join's ON is now dropped before the join is planned,
as PostgreSQL's `eval_const_expressions` drops a TRUE argument of an AND:
`select s.a, e.a from s join e on true and s.a > 29` prints a plain Nested
Loop over `Seq Scan on s` with `Filter: (a > 29)`, and `s left join e on
true` a Nested Loop Left Join with no Join Filter, as PostgreSQL 15.19
does, where both used to print `Join Filter: true`. EXPLAIN ANALYZE of a
LEFT Nested Loop with no Join Filter now counts its children's actual rows
and loops too, which it left blank before. The rows were right in both
cases.

#### Fixed

- Fixes RP-394 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join whose ON has a constant true conjunct])
- Fixes RP-396, found while working RP-394 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left join on true])

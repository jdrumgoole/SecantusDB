### Rust PG server: an inner join's one-table ON conjunct filters its table's scan

An ON conjunct of an inner join that reads one table only is now that
table's restriction, run and printed on its scan, as PostgreSQL's
`distribute_qual_to_rels` places it: `select * from s join e on s.a < e.a
and s.a > 25` prints `Join Filter: (s.a < e.a)` over a Materialized `Seq
Scan on s` with `Filter: (a > 25)`, as PostgreSQL 15.19 does, where the
join used to hold both conjuncts and rescan the bare table.

#### Fixed

- Fixes RP-391 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join's one-table ON conjunct])

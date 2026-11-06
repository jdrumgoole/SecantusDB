### Rust PG server: an inner join whose every ON conjunct reads one table is a plain Nested Loop

When every ON conjunct of an inner join reads one table, each is now that
table's scan Filter and the join keeps no clause, as PostgreSQL's
`distribute_qual_to_rels` leaves it: `select * from s join e on s.a > 25
and e.a > 990` prints a Nested Loop with no Join Filter over `Seq Scan on
e` with `Filter: (a > 990)` and a Materialized `Seq Scan on s` with
`Filter: (a > 25)`, as PostgreSQL 15.19 does, where the join used to hold
the whole ON and scan both tables bare.

#### Fixed

- Fixes RP-392 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join whose ON conjuncts each read one table])

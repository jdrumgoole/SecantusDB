### Rust PG server: an outer join's ON conjunct over its nullable side filters that side's scan

A LEFT (or RIGHT) join's ON conjunct that reads only the nullable side now
runs in that side's scan, as PostgreSQL's `distribute_qual_to_rels` places
it, rather than in the join's Join Filter: `select * from s left join e on
s.a * 40 > e.a and e.b = 'v3'` prints `Join Filter: ((s.a * 40) > e.a)`
over a Materialized `Seq Scan on e` with `Filter: (b = 'v3'::text)`, and
EXPLAIN ANALYZE counts that scan's 100 rows and its `Rows Removed by
Filter: 900`, as PostgreSQL 15.19 does. The scan used to return all 1,000
rows on every loop, and the join test the conjunct per pair. With the
filter in place, these joins now print PostgreSQL's estimates where they
printed zeros (a Nested Loop Left Join over an inequality, a Hash Right
Join). Taken where the nullable side is one table and the conjunct
qualifies its columns; the rows were right before and are unchanged.

Found while working it, and filed: RP-397 (a conjunct over a table inside
an inner join on the nullable side), RP-398 (a joined scan's Filter keeps
written order), RP-399 (a BETWEEN prints as a nested AND), RP-400 (a
join whose WHERE equates a column to two constants still scans) and
RP-401 (a LEFT join on an indexed key prints a Hash join where
PostgreSQL merges through the index).

#### Fixed

- Fixes RP-395 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a left nested loop over an expression filter and a filtered inner])

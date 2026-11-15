### EXPLAIN of an UPDATE or DELETE pulls up a one-row constant subquery

PostgreSQL pulls a FROM subquery of constants up into the statement, so
`delete from k using (select 50 as a) t where k.a = t.a` plans as
`delete from k where k.a = 50`, and the key's index answers it. The Rust
PostgreSQL server's EXPLAIN of such an UPDATE ... FROM or DELETE ... USING
now prints that Index Scan, where it printed a Seq Scan of the target.

#### Fixed

- `dml_pullup::pull_up_constant_subqueries` substitutes a one-row
  constant subquery's columns before an UPDATE or DELETE is planned for
  EXPLAIN. A DELETE ... USING a table still prints no join (RP-435 stays
  open for that half).
  Fixes part of RP-435 (tests/backlog/test_rp_dml_from.py::test_explain_pulls_up_a_constant_subquery)

### EXPLAIN of an UPDATE ... FROM or DELETE ... USING prints its join

PostgreSQL plans an UPDATE with a FROM list, or a DELETE with a USING
list, as a join of the target with those tables, and its ModifyTable sits
on that join. The Rust PostgreSQL server's EXPLAIN printed a Seq Scan of
the target alone; it now plans the statement's join the way the same
SELECT is planned, so `explain delete from k using e where k.a = e.a`
prints the Hash Join, its Hash Cond and both scans, with PostgreSQL's
costs. The statement itself still runs as before.

#### Fixed

- `dml_pullup::dml_join_as_select` explains an UPDATE ... FROM or DELETE
  ... USING (without ANALYZE) as `SELECT * FROM <target>, <from items>
  WHERE ...`, under a `Delete on` / `Update on` node costing what the join
  does with `rows=0 width=0`. The join's widths are still the SELECT's
  (RP-435 stays open for them); a join of tables never analysed is
  unpriced (RP-1019, filed).
  Fixes part of RP-435 (tests/backlog/test_rp_dml_from.py::test_explain_plans_the_join)

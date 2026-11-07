### Rust PG server: an UPDATE, DELETE or INSERT reads the CTEs of its own WITH

A write's WITH is now in scope for the whole write, as PostgreSQL's
`transformWithClause` puts it. Before, `with t as (select b from e) update
k set a = a + 100 from t where k.b = t.b`, `... update k ... where a in
(select a from t)`, `with t as (select 50 as a) delete from k using t where
k.a = t.a` and a data-modifying CTE reading an earlier one each answered
42P01 `relation "t" does not exist`, where PostgreSQL 15.19 runs them.

#### Fixed

- A CTE named in an UPDATE's FROM or a DELETE's USING is read as the
  FROM-subquery it stands for, so its columns also count in the check for
  an ambiguous unqualified name (42702), as in PostgreSQL.
- A subquery anywhere in an UPDATE or DELETE (WHERE, SET, RETURNING), or in
  an INSERT's VALUES or RETURNING, sees the statement's CTEs.
- A `WITH RECURSIVE` on an UPDATE, a DELETE or an INSERT ... SELECT is run
  as a SELECT's is. On an UPDATE or a DELETE it was 42P01.
- A data-modifying CTE (`with t as (...), u as (delete from k where a in
  (select a from t) returning a) select * from u`) sees the CTEs written
  before it.
- Fixes RP-432
  (tests/backlog/test_rp_analysis_order.py::test_update_and_delete_read_their_ctes,
  with tests/backlog/test_rp_analysis_order.py::test_writes_read_their_ctes_everywhere
  beside it), and a crate regression test
  (`secantus-pgplan` `tests::a_write_reads_the_ctes_of_its_own_with`).
- Filed RP-435: EXPLAIN of an UPDATE ... FROM or a DELETE ... USING prints a
  Seq Scan of the target alone, with no join and no Index Cond.
- Filed RP-436: an UPDATE ... FROM or DELETE ... USING reports an ambiguous
  name (42702) ahead of a missing column (42703) that PostgreSQL meets first.

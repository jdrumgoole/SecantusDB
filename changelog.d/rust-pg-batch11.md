### Rust PostgreSQL server: backlog batch 11

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A cursor over a join scrolls when PostgreSQL's does.** A cursor
  declared with neither SCROLL nor NO SCROLL can scan backward only when
  its plan can: a Sort on top can, a join cannot. Over `a JOIN b ON a.x =
  b.y ORDER BY a.x`, PostgreSQL picks the plan cheapest for a cursor's
  tenth of the rows, and that is a merge join in key order -- no backward
  FETCH -- or a Sort over a hash join. The server now prices both as
  PostgreSQL does (`secantus-pgplan/src/joincost.rs`: hash and merge join
  costs, join selectivity, hash bucket sizes), where every such cursor
  used to scroll. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_join_cursor_ordered_by_its_key_cannot_scroll).
- **A join of two tables on one equality returns its rows in
  PostgreSQL's order.** The order shows when keys repeat: a merge join
  reads each side in key order, equal keys as PostgreSQL's quicksort
  leaves them (or in heap order through an index), and a hash join
  returns a probe's matches in its bucket's order, last inserted first.
  The server picks the method by the same costs and puts its inputs in
  that order; such a join of up to 200,000 rows a side is no longer
  streamed. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_many_to_many_merge_join_leaves_ties_as_its_sorts_do).
- **ROLLBACK TO a savepoint closes the cursors declared after it**, as
  PostgreSQL's subtransaction abort does: a FETCH from one is 34000. They
  had survived the rollback. Fixes RP-045
  (tests/backlog/test_rust_postgres_gaps.py::test_rollback_to_savepoint_closes_its_cursors).
- **A prepared FETCH follows its cursor's columns.** psycopg prepares a
  statement it runs five times, so one FETCH serves cursors re-declared
  with other columns; after a savepoint it answered 0A000 `cached plan must
  not change result type`. A FETCH's result is its cursor's, which
  PostgreSQL describes afresh. Fixes RP-046
  (tests/backlog/test_rust_postgres_gaps.py::test_prepared_fetch_follows_the_cursors_columns).

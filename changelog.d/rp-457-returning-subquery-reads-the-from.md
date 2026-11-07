### A RETURNING subquery can read an UPDATE's FROM or a DELETE's USING

On the Rust PostgreSQL server, a subquery in the RETURNING list of an
`UPDATE ... FROM` or a `DELETE ... USING` can now read the FROM or USING
items, which in PostgreSQL belong to the write's own level. `update fe set
b = 'w' from fs returning (select fs.a from fe limit 1)` used to fail with
42P01 `missing FROM-clause entry for table "fs"`. It now returns what
PostgreSQL 15.19 returns, with or without an alias, and for a bare column
name, a nested subquery, a derived table or a join in the FROM.

#### Fixed

- `secantus-pgplan`: a RETURNING item that reads a FROM item only inside a
  subquery is now rewritten through the FROM, like an item that reads one
  directly. A qualifier the subquery's own FROM does not name counts as an
  outer reference, and so does a bare name that none of the subquery's own
  tables has. Fixes RP-457
  (tests/backlog/test_rp_subquery_scope.py::test_returning_subquery_reads_the_from),
  with the regression test
  tests/test_rust_pgserver_slice.py::test_returning_subquery_reads_the_from.

### Rust PG server: a COMMENT ON FUNCTION is seen by its own transaction

A comment on a function, procedure or routine is now part of the
transaction that made it, as PostgreSQL's `pg_description` row is: the
transaction reads it back with `obj_description`, ROLLBACK and ROLLBACK TO
SAVEPOINT undo it, and another session sees it only once it commits.

#### Fixed

- `begin; comment on function gc(int) is 'w'; select
  obj_description('gc(int)'::regprocedure, 'pg_proc')` answered NULL until
  the commit. The comment was written to the function's record straight to
  storage, past the transaction's catalog overlay that the function catalog
  is read through; it is now rewritten through that overlay, as ALTER
  FUNCTION rewrites the record.

Fixes RP-518 (tests/backlog/test_rp_found_rp514.py::test_function_comment_seen_in_its_transaction)

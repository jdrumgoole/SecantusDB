### A built-in routine takes a comment

`COMMENT ON FUNCTION abs(int) IS '...'` is recorded the way PostgreSQL
records it: as a `pg_description` row of `pg_proc`, keyed by the oid of the
one overload named, so `obj_description` reads it back and `abs(bigint)`
keeps its own. Before, every comment on a built-in routine was refused as
not supported. Comments made inside a transaction block are now also read
back by `obj_description` in that block, which a COMMENT ON INDEX was not.

#### Fixed

- COMMENT ON FUNCTION / ROUTINE on a built-in overload is stored by its
  `pg_proc` oid and listed in `pg_description` (classoid `pg_proc`);
  `IS NULL` removes it and a rollback undoes it. initdb's own built-in
  descriptions are still not served.
- `obj_description` read the comments outside the open transaction block,
  so a COMMENT ON INDEX (or on a built-in routine) made in the block was
  not seen until it committed.
- Fixes RP-519 (tests/backlog/test_rp_found_rp514.py::test_builtin_routine_takes_a_comment);
  regression tests `test_builtin_routine_comment_in_pg_description`,
  `test_builtin_routine_comment_rolls_back`,
  `test_index_comment_seen_in_its_transaction` in the same file.

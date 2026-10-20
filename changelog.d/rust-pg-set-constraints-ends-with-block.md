### Rust PostgreSQL server: `SET CONSTRAINTS` ends with its transaction

After `BEGIN; SET CONSTRAINTS ALL DEFERRED; COMMIT;`, the connection went
on deferring constraint checks in every later transaction block. An insert
that broke a `DEFERRABLE INITIALLY IMMEDIATE` foreign key was accepted
instead of failing at once with 23503, as PostgreSQL answers. Only a
`ROLLBACK` reset the mode. Every path that ends a transaction now clears
it, along with the queued deferred checks, after COMMIT has run them.

#### Fixed

- `secantus-pg`: Fixes RP-052
  (`tests/backlog/test_rp_found_2026_10_19.py::test_set_constraints_ends_with_its_transaction`).
  The deferred foreign keys, deferred constraint triggers and
  `SET CONSTRAINTS` modes move from three `PgHandler` fields into
  `txn_state::DeferredChecks`, cleared by every transaction-end path:
  explicit COMMIT / ROLLBACK / PREPARE, `commit_implicit` (including a
  procedure's own COMMIT) and `rollback_implicit` (part of RP-049).

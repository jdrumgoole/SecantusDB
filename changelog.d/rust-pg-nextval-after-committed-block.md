### Rust PostgreSQL server: a ROLLBACK no longer undoes `nextval` after a block that created a sequence

After `BEGIN; CREATE SEQUENCE s; COMMIT;`, every later transaction block on
the same connection advanced the sequence inside its own transaction, so a
`ROLLBACK` undid the `nextval` and the next call handed out the same value
again. PostgreSQL never rolls a sequence back. The cause was that the
connection kept a record of the sequences the committed block had written,
and an explicit `COMMIT` or `ROLLBACK` never cleared it. The implicit
transaction paths did. All four paths that end a transaction now clear the
same state in one place.

#### Fixed

- `secantus-pg`: Fixes RP-050
  (`tests/backlog/test_rp_found_2026_10_19.py::test_nextval_is_not_rolled_back_after_a_committed_block`).
  The open transaction's uncommitted tables, types and sequences move out of
  three `PgHandler` fields into `txn_state::PendingCatalog`, which has one
  `clear` for every transaction-end path (part of RP-049).

### Rust PostgreSQL server: GRANT and REVOKE take effect inside the transaction block

A privilege granted or revoked inside `BEGIN ... COMMIT` now applies to the
statements that follow it in the same block, as on PostgreSQL 15.

#### Fixed

- `GRANT` / `REVOKE` inside a transaction block took no effect until
  `COMMIT`: the privilege check, and `has_table_privilege`, read committed
  state rather than the block's own writes. Fixes RP-020
  (tests/backlog/test_rust_postgres_gaps.py::test_grant_and_revoke_take_effect_inside_a_transaction_block).

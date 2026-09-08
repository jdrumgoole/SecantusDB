### The Rust PostgreSQL server honours transaction characteristics

psycopg exposes a connection's `isolation_level`, `read_only` and `deferrable`
as first-class attributes, and applies them by tacking the transaction
characteristics onto the `BEGIN` it emits before each block
(`BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY DEFERRABLE`), reading them back
through `current_setting('transaction_isolation')` and friends. The Rust
`secantusd-pg` server refused those characteristics, so sixteen of psycopg's
`test_set_transaction_param_*` conformance tests failed on the very first
statement.

The server now parses the characteristics on `BEGIN` / `START TRANSACTION`,
`SET TRANSACTION`, and `SET SESSION CHARACTERISTICS AS TRANSACTION`, and reflects
them in the `transaction_*` / `default_transaction_*` GUCs a client reads back.
SecantusDB is single-node, so the isolation level is accepted and reported but
not enforced — every level behaves as the one snapshot the storage engine
offers, which is exactly what a single-node PostgreSQL does. Opening a block
resets `transaction_*` to the session default and overlays the named modes;
ending it reverts to the default, matching PostgreSQL 14 exactly. A companion
fix lets `set_config($1, $2, false)` plan over the extended protocol, where the
name parameter is still unbound at DESCRIBE time.

All sixteen `test_set_transaction_param_*` tests now pass.

#### Added

- `secantus-pgplan`: `TransactionModes` on `BEGIN` / `START TRANSACTION`, and the
  `SetTransaction` / `SetSessionCharacteristics` statements for the two `SET`
  forms (`SET TRANSACTION`, `SET SESSION CHARACTERISTICS AS TRANSACTION`).
- `secantus-pgserver`: applies the modes to the `transaction_*` GUCs for the life
  of a block and reverts on commit / rollback; a `default_transaction_deferrable`
  default GUC.

#### Fixed

- `secantus-pgplan`: `set_config(name, value, is_local)` folds a NULL name to
  NULL during a DESCRIBE instead of erroring, mirroring `current_setting`, so a
  fully-parameterised `set_config` round-trips over the extended protocol.

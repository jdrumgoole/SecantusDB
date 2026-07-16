### SQL server: a round of protocol-fidelity fixes from the psycopg gauge

A batch of small wire-protocol and error-surface divergences, each found by
running psycopg's own test suite against the server: `to_regtype` now accepts
double-quoted identifiers (psycopg's `TypeInfo.fetch(conn,
sql.Identifier("text"))` was returning None); garbage input (`"wat"`) raises
a real syntax error (42601) instead of feature-not-supported, so clients map
it to ProgrammingError; a non-numeric string bound to an integer or float
column surfaces `22P02 invalid input syntax` instead of an internal error;
COPY TO STDOUT sends one CopyData message per row like a real server (a
single all-rows blob made every row after the first vanish in psycopg's
`Copy.rows()`); a client's CopyFail aborts the enclosing transaction
(INERROR); a bare `VALUES (…)` answers extended-protocol Describe with its
row shape instead of NoData-then-DataRows (a protocol violation that crashes
libpq's stream mode); RowDescription reports fixed-width types' `typlen` and
encodes column names in the client's encoding; `pg_sleep()` sleeps;
`pg_tables` exists; and the transaction-characteristics GUCs
(`transaction_isolation` etc.) report their honest single-node constants.

Together these clear ~60 tests across psycopg's `test_typeinfo` (18 → 0),
`test_cursor_common` (27 → 3), `test_copy` (37 → 26) and `test_column`
(42 → 35) files.

#### Fixed

- `typemap.oid_for_regtype` / `planner._to_regtype`: double-quoted identifier
  resolution with Postgres case rules (quoted names keep case; built-ins only
  match lowercase).
- `engine.py`: bare expression statements → `42601`; extended-protocol
  Describe of `VALUES` returns the row shape.
- `typemap.coerce`: int/float coercion failures raise `22P02` (as an
  exception that is also a `ValueError`, so soft-fallback callers keep their
  behaviour).
- `pgserver.py`: COPY OUT chunks per row; CopyFail marks the transaction
  failed.
- `pgwire.row_description`: static `typlen` table; client-encoding column
  names (threaded from both the simple and extended paths).
- `functions.py`: `pg_sleep` (capped at 30s — our connection threads have no
  cancel path); `session.py`: transaction-characteristics GUC defaults.
- `virtual.py`: the `pg_tables` system view.

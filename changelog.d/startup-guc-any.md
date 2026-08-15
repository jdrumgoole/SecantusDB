### Any run-time GUC works as a startup parameter

The startup packet's parameter list applied only the reportable GUCs
(TimeZone, client_encoding and friends); everything else was dropped.
Real PostgreSQL applies any run-time GUC sent at startup as the session
default — pgx's `target_session_attrs=read-write` probe relies on it,
shipping `default_transaction_read_only=on` in the startup packet and
expecting `SHOW transaction_read_only` to answer `on` so the validator
can reject the connection.

All startup parameters are now applied as session GUCs (`user` /
`database` / `options` / `replication` and the `_pq_.*` protocol options
excepted), with the existing TimeZone and client_encoding
canonicalization preserved.

#### Fixed

- `sql/pgserver.py`: the startup-parameter loop applies every GUC, not
  just the reportable set.

### Rust pgserver: transaction settings psycopg reads

psycopg reads a handful of transaction GUCs to learn a connection's defaults,
and the Rust PostgreSQL server answered `42704` (unrecognized parameter) for
all of them, which failed a swathe of cursor and connection tests before they
could begin. The server now reports the fixed values a single-node server
gives: `max_prepared_transactions` is `0` (no two-phase commit),
`transaction_isolation` / `default_transaction_isolation` are `read committed`,
and `transaction_deferrable` / `default_transaction_read_only` are `off`.

#### Added
- `SHOW` / `SET` recognise `max_prepared_transactions`,
  `transaction_isolation`, `default_transaction_isolation`,
  `transaction_deferrable` and `default_transaction_read_only`.

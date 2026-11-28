### `client_min_messages` decides which notices reach the client

`secantusd-pg` stored `client_min_messages` and reported it, but sent every
NOTICE whatever its value. It now decides each message as PostgreSQL's
`should_output_to_client` does, at the moment the message is raised: a
message at or above the setting goes out, an INFO always does.

#### Fixed

- After `set client_min_messages = warning`, a NOTICE (`drop table if
  exists` of a missing table, a `raise notice`, text search's stop-word
  notice) is no longer sent; at `error` a WARNING is withheld too, and an
  INFO still arrives, as on PostgreSQL 15.19.
- The value in force at the raise counts: a function's own `SET
  client_min_messages = warning` clause withholds the notices raised inside
  it, and a DO block that changes the setting part-way sends what it raised
  before.
- `raise debug` and `raise log` were never sent; they now reach the client
  when `client_min_messages` is `debug1` (or lower) and `log` respectively.
- The hidden spellings `debug` (stored as `debug2`) and `info` of
  `client_min_messages`, and `debug` of `log_min_messages` and
  `log_min_error_statement`, were refused as invalid values.
- Fixes RP-1092 (`tests/backlog/test_rp_found_rp860.py::test_client_min_messages_withholds_a_notice`;
  regression tests in `tests/test_rust_pg_client_min_messages.py`).
  Files RP-1230: at `debug5` no `StartTransaction` / `CommitTransaction`
  trace is sent.

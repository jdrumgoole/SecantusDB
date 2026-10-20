### Storage: an insert does its per-document work before taking the collection's lock

Concurrent writers to one collection take turns on its write lock. With three
connections inserting into one table, that lock was held about 88% of the
time, so only work done under it set the rate. Each document's decode, an
`_id` minted for one that has none, its `_id` key and the oplog entry's
`{_id}` now happen before the lock is taken. An insert's oplog entries also
skip the check of whether each names the unlogged `local` database or a
profile collection, which the insert's own namespace answers. psycopg's
`test_type_error_shadow` script (150,000 prepared UUID inserts from three
connections) went from 8.2 s to 7.6 s in a debug build (PostgreSQL 15.19:
8.0 s). Decided by Joe, 2026-10-20: keep optimising (RP-014).

#### Changed

- `secantus-storage` (`insert_chunk`): documents are prepared before the
  collection lock. `emit_oplog_entries_logged` writes entries already known to
  be logged. This is the storage both Rust servers link, so the MongoDB
  server's inserts take the same path.
- `secantus-pg`: `with_expr_fields` asks storage's cached index list for an
  expression index before reading the table's definition, and an INSERT and
  `check_deferrable_uniques` share the cached definition (`lookup_shared`)
  rather than copying it. Less CPU per row, though not visible in the script,
  whose time the lock sets.

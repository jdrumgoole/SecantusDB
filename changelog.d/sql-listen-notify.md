### LISTEN / NOTIFY: a notification delivered twice

#### Fixed

- `pg_notify()` sent its notification **twice** when the call carried a
  parameter. Describe evaluates a FROM-less `SELECT` to learn its column shape,
  and the table that exists so a volatile call's shape is derived statically
  instead of run — `engine._VOLATILE_FN_TAGS` — was missing `pg_notify`, so
  Describe sent the notification and Execute sent it again. Only the extended
  protocol reaches it, because a parameter is what stops a driver using the
  simple one, so the literal spelling always looked correct. `nextval`,
  `pg_sleep`, the advisory locks, and `INSERT` with parameters were checked and
  were already correct.
- An identical `(channel, payload)` signalled more than once in a transaction
  is now delivered once, as PostgreSQL does, so a loop that notifies per row
  wakes a listener once rather than once per row. Distinct payloads on the same
  channel are still all delivered. Both `NOTIFY` and `pg_notify()` collapse —
  the two spellings of one operation had different semantics.
- A payload of 8000 bytes or more raises `22023 payload string too long`
  instead of being delivered. 7999 is accepted.

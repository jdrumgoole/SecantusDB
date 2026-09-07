### Rust pgserver: timestamptz columns

`timestamptz` is a real column type now. It is stored as a UTC INSTANT — the
same date-plus-microsecond-companion carrier a `timestamp` uses — and rendered
in the session's zone on the way out, so the same stored instant reads back
correctly under any `SET timezone` rather than in whichever zone happened to
write it. This closes a deliberate refusal that had stood because storing
session-rendered text was a wrong answer for every other session.

Making it correct end to end also meant the server now reports GUC changes:
after a `SET`, it emits a `ParameterStatus` for the variables PostgreSQL marks
GUC_REPORT (`TimeZone`, `DateStyle`, `client_encoding`, …). libpq and psycopg
track the session `TimeZone` from that message and re-express a timestamptz in
it — without the report a stored instant displayed in the client's stale
startup zone.

#### Added
- `timestamptz` column type (oid 1184): UTC-instant storage, session-zone
  rendering, sub-millisecond precision, and the `::timestamptz::text` cast.
- `ParameterStatus` reports for GUC_REPORT variables on `SET` / `RESET`.

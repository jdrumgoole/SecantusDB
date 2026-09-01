### Date, time and timestamp columns on the Rust PostgreSQL server

`date` and `time` now work as column types and as casts. They are stored in the
same canonical text form the Python server uses — the two servers share one
database, so the representation is a contract rather than a choice — but are
reported over the wire with their real type identifiers, which is what lets a
client hand back a date object rather than a string.

Writing a test that used `date` as a column type, rather than only as a cast,
turned up something larger and unrelated to dates. A value assigned to a column
was stored exactly as written instead of being converted to the column's type,
so `INSERT INTO t (d) VALUES ('2026-9-1')` stored `2026-9-1` and a client
reading it back could not parse it. PostgreSQL converts on assignment; now so do
we, for `INSERT` and `UPDATE` alike and for every type, not just dates.

Two error codes that are easy to conflate are kept apart, because PostgreSQL
keeps them apart: a value that is not a date at all is one error, and a
well-formed date naming a day that does not exist — the thirtieth of February —
is another.

Timestamps needed more care than the other two. PostgreSQL keeps microseconds;
the underlying document format keeps only milliseconds. The Python server
already solved this by storing the truncated time and keeping the lost
microseconds in a hidden companion field beside it, and this server now writes
exactly the same thing — so a timestamp written by one is read back at full
precision by the other. The rule that makes it safe is that every write must
either set that companion or remove it: leaving a stale one behind would report
a time nobody ever stored, which is worse than losing the precision would have
been. Overwriting a precise time with a whole-millisecond one, and back again,
is covered by a test for exactly that reason.

#### Added

- `date`, `time` and `timestamp` as column types and cast targets, accepting the
  spellings PostgreSQL accepts and storing the single canonical form it stores.
- Microsecond precision for timestamps, stored compatibly with the Python
  server so a value written by either is read correctly by both.

#### Fixed

- Values assigned by `INSERT` or `UPDATE` were stored as written rather than
  converted to the column's declared type.

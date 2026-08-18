### Timestamps keep their microseconds

MongoDB has no sub-millisecond date. A BSON date is a count of whole
milliseconds, and the other date-like type — `Timestamp` — is coarser still,
being seconds plus a counter for replication ordering. A Postgres `timestamp`
carries microseconds, so inserting `12:00:00.123456` and reading it back gave
`12:00:00.123000`: the last three digits were dropped, silently, with no error.

Those digits are now kept. The BSON date still holds the whole milliseconds —
a Mongo client reading the same collection sees exactly the date it always saw
— and the leftover microseconds ride alongside it in a hidden `__us_<field>`
companion, written only when they are non-zero, so a timestamp that lands on a
whole millisecond adds nothing to the document. The companion is not a column:
it never appears in `SELECT *`, in `information_schema`, or among the columns
reflected from a schema-on-read collection.

The rule that matters if you are reading documents directly: every write
resolves the companion. Updating a timestamp to a whole-millisecond value
*removes* it rather than leaving the previous value's microseconds attached,
because a stale remainder would report a time that was never stored. A
remainder outside 0–999 is ignored on read rather than trusted.

One limitation is unchanged and is now documented rather than implied:
`WHERE` and `ORDER BY` on a timestamp column still compare whole milliseconds,
because that is what the stored date holds. A sub-millisecond literal matches
nothing, and rows within the same millisecond sort in an unspecified order —
exactly as before. Reads are now precise; predicates are not yet.

#### Fixed

- `timestamp` / `timestamptz` columns round-trip microseconds through
  `INSERT`, `UPDATE`, `SELECT` and `RETURNING` instead of truncating to
  milliseconds.

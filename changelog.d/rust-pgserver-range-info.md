### RangeInfo.fetch, and a plain join

`RangeInfo.fetch` works now — psycopg's range-type discovery, unmodified. Its
query is `pg_type` joined to `pg_range` on `rngtypid`, but *without* the
aggregate wrapper `EnumInfo` uses: a plain top-level JOIN in an ordinary
SELECT. So the join source, which the enum work put on the aggregate planner,
now lives on plain selects too, and `pg_range` joins the virtual catalog — six
rows, each builtin range type paired with its element type's oid, read from the
same table the range casts use so the two can never disagree.

#### Added

- The `pg_range` virtual table (`rngtypid`, `rngsubtype`).
- A top-level two-table JOIN in a plain (non-aggregate) SELECT, so
  `RangeInfo.fetch` resolves.

### Timestamp arrays keep their microseconds

A `timestamp[]` or `timestamptz[]` column on the Python PostgreSQL server
stored every element to the millisecond: `10:00:00.826829` came back as
`10:00:00.826`, with no error. The same array written as an expression was
exact, so the loss was only in storage.

A plain timestamp column keeps its sub-millisecond part in a hidden field beside
the stored date. Array columns never had one, and each element of the array is
stored as a date too. They now keep a parallel list of remainders, written only
when some element needs one, and cleared when an update no longer does.

Found by psycopg's random-data COPY tests, which failed whenever a draw put
microseconds inside an array.

#### Fixed

- `sql/subms.py`: `split` / `merge` work element by element on arrays (nested
  arrays too); `carries_subms` names the column types that keep the companion.
- `sql/planner.py`, `sql/executor.py`: `INSERT`, `COPY FROM`, `UPDATE` and the
  whole-row read paths (including `COPY TO`) use it for array columns.

#### Testing

- `tests/test_pg_timestamp_array_subms.py`: parameters with NULL elements, a
  two-dimensional literal, updates that set and clear remainders, and `COPY`
  in and out in text and binary.

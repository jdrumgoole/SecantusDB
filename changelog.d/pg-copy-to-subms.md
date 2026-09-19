### `COPY TO` exports a timestamp's microseconds

`COPY <table> TO` on the Python PostgreSQL server cut every `timestamp` and
`timestamptz` down to the millisecond: a stored `10:00:00.412661` was exported
as `10:00:00.412`, in both the text and binary formats. `SELECT` and
`COPY (SELECT …) TO` were exact. `COPY TO` is how a table is exported or backed
up, so every export taken that way was quietly less precise than the data.

The server keeps a timestamp's sub-millisecond part beside the stored date, and
the table form of `COPY TO` read the date without it. It now reads each value
the way `SELECT` does. Found by psycopg's `test_copy_table_across`, which passed
or failed depending on whether its random timestamps had microseconds.

#### Fixed

- `sql/engine.py`: `copy_extract` and `copy_extract_raw` read each column through
  `executor._with_subms`, the same merge `SELECT` uses.

#### Testing

- `tests/test_pg_copy_to_subms.py`: text and binary exports, and a table copied
  across through `COPY TO` / `COPY FROM` in both formats. All four fail without
  the fix.

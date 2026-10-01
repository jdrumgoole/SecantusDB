### Python PostgreSQL server: fixes from a full PostgreSQL corpus sweep

Running every corpus in `tools/probes/pg_corpora/` against the Python PG server turned up these divergences from PostgreSQL 15. They are fixed, and the sweep total drops from 2,985 to 2,955 with no corpus getting worse.

#### Fixed

- A join whose alias equals a column of the FROM table (`FROM a o JOIN a n`) returned the whole joined row for `o.n`, silently. It now returns the value.
- `IS [NOT] DISTINCT FROM` works in `WHERE`, `JOIN ... ON`, `UPDATE` and `DELETE`. It answered `0A000` there before.
- `string_agg(<expression>, ...)` over a join now works. It answered `0A000 expected a column`.
- `CREATE OR REPLACE VIEW` refuses to drop, rename or retype a view column with `42P16`, as PostgreSQL does. It used to replace the view silently.
- Dates with a month name (`'Jan 5, 2020'`, `'5 Jan 2020'`, `'2020-Jan-05'`) are accepted.
- A date with a day past its month's end (`'2020-02-30'`, `'Feb 29 2021'`) answers `22008`, where it answered `22007`.
- `SELECT t FROM t` describes its column with the table's row type rather than generic `record`, so a client reads it as it does from PostgreSQL.
- `pg_trigger` lists triggers (it was always empty), including the deferrability of constraint triggers.
- `SET CONSTRAINTS` naming no constraint answers `42704`.
- Transition tables with one name for OLD and NEW answer `42P17`.

### Chasing the JDBC driver's failures turns up six real server bugs

Working the pgjdbc conformance gauge's failure clusters took it from 89.7% to
**92.4%** of the driver's `jdbc2` suite — but the point is what the failures
were hiding. Six of them were genuine correctness bugs, two of which produced
wrong answers rather than errors.

The starkest: an **ungrouped aggregate returned no rows when its WHERE
excluded everything**. `SELECT count(*) WHERE 1=2` answered "no rows" where
PostgreSQL answers `0`, and `SELECT max(3) WHERE 1=2` answered nothing where
PostgreSQL answers one NULL row. This was verified against a real PostgreSQL
14.13 rather than from memory — and it means `SELECT 0/count(*) WHERE 1=2`
now raises division-by-zero, which is precisely how pgjdbc's batch tests
inject a runtime failure. A pre-existing test had encoded the wrong
behaviour; it has been corrected with the verification noted in place.

Also fixed: BC-era timestamps are accepted with the era marker either side of
the zone offset (pgjdbc sends `0101-01-01 BC +00`, PostgreSQL's datetime
input is field-order flexible), and a BC value stored in a `date` column no
longer silently loses its era and becomes an AD date. `time` and `timetz`
accept a full timestamp and keep the time-of-day, as PostgreSQL does.
Multi-dimensional enum arrays (`flag[][]`) no longer crash the server, and
nested arrays render with nested braces instead of quoted JSON. `x = ANY(…)`
works in per-row evaluation, `current_schemas()` is implemented, and
`ALTER DATABASE … SET` stores database-level GUC defaults applied to new
sessions with PostgreSQL's precedence. Finally, extended-protocol Describe no
longer needs parameter *values*: `SELECT $1::inet` has a shape fixed by its
cast target.

#### Added

- `sql/scalar.py`: `current_schemas(include_implicit)`, `x = ANY(<array>)` in
  per-row evaluation, `pg_encoding_to_char`.
- `sql/engine.py` + `sql/catalog.py` + `sql/session.py`: `ALTER DATABASE …
  SET / RESET [ALL]` database-level GUC defaults, merged into new sessions
  (explicit session settings still win).
- `sql/engine.py`: value-free Describe fallback for cast projections over
  unbound parameters.

#### Fixed

- `sql/planner.py`: an ungrouped aggregate now yields exactly one row when the
  WHERE excludes the implicit row (COUNT 0, others NULL) — previously zero
  rows, a wrong answer. Verified against PostgreSQL 14.13.
- `sql/datetimes.py`: the BC era marker is accepted before or after a zone
  offset; a BC/out-of-range value with a time part keeps its era in a `date`
  column (previously became an AD date); `time` / `timetz` accept a full
  timestamp and a trailing offset.
- `sql/scalar.py`: multi-dimensional enum arrays (`flag[][]`) raised an
  internal error; labels are now validated at every depth.
- `sql/typemap.py`: nested array text rendering inferred its element type from
  the outer list, rendering sub-arrays as quoted JSON instead of nested braces.

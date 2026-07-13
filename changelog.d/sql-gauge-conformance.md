### SQL server: client_encoding, wire-protocol fixes, and binary-format hardening from the psycopg gauge

Running psycopg 3's own unmodified test suite against the SQL server
(`tasks/sql-gauges-plan.md`) surfaced a batch of wire-protocol and
type-handling divergences beyond the type-OID work. The headline is
`client_encoding` support: the server now honours the startup parameter and
`SET client_encoding` (LATIN1/LATIN2/LATIN5/LATIN9, WIN1250-1252,
SQL_ASCII pass-through), converting query text, text and binary parameters,
text and binary results, arrays, COPY data, and error messages at the wire
boundary while the engine stays UTF-8 throughout. Alongside it, a real
protocol-ordering bug: Describe answered NoData for DML with RETURNING while
Execute then emitted DataRows — a violation that crashed psycopg's pipelined
`executemany`. The measured effect on the fixed psycopg-gauge subset
(six files, psycopg 3.3.4): 409 → 637 passed of 979 (42% → 65%) across this
and the preceding type-OID release.

#### Added

- `client_encoding` (startup parameter and `SET`, with canonical
  ParameterStatus reporting and `22023` on unknown encodings); an
  untranslatable result character raises `22P05` like Postgres instead of
  degrading to `?`, and a NUL byte in a text parameter is rejected with
  `22021`.
- Quoted built-in type names in DDL (`CREATE TABLE t (c "cidr")`, the form
  psycopg's fixtures emit via `sql.Identifier`) resolve as built-ins —
  including array spellings — instead of failing as undeclared enums.

#### Fixed

- `pgextended.py`: Describe on INSERT/UPDATE/DELETE/MERGE … RETURNING
  answers with the RETURNING columns' RowDescription (was NoData followed by
  DataRows — a protocol violation).
- `engine.py`: Describe on a set-returning row source (`FROM
  generate_series(…)` / bare `SELECT generate_series(…)`) resolves the
  result shape instead of erroring — this is what failed every
  `cursor.stream()` (libpq single-row mode) call.
- Array round-trips, all six param/result format combinations: a binary
  array parameter's Python list is rendered as a Postgres array literal
  (was the Python `repr`); the array-literal parser strips only Postgres'
  whitespace set (`\x1c`–`\x1f` are `str.isspace()` to Python but data to
  Postgres); the renderer quotes every whitespace character; binary array
  elements coerce to native values (`bytea` hex, `bool` `'t'/'f'`) before
  encoding. chr(1)–chr(255) plus `€` now round-trip byte-exact in text and
  bytea arrays.
- Binary `numeric` handles `±Infinity` in both directions (signs
  `0xD000`/`0xF000`; encoding previously crashed, decoding produced
  garbage).

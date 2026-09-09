### The Rust PostgreSQL server passes psycopg's array and cursor suites

psycopg's `tests/types/test_array.py` and `tests/test_cursor_common.py` went
from 52 failures to 2 against the Rust PG server (`secantusd-pg`), every
change measured against PostgreSQL 16 first. The array literal scanner is now
a port of `array_in` (only the six `array_isspace` characters are trimmed,
quotes and backslashes are handled at the level they occur, the `[lo:hi]=`
decoration is validated, and each malformed shape is the 22P02 PostgreSQL
gives it); multidimensional arrays round-trip in text and binary both ways;
`||` beside an array follows `array_cat` / `array_append` / `array_prepend`.
`INSERT … RETURNING` returns named, computed and `*` columns typed from the
row — including `executemany(..., returning=True)` — and `serial` /
`bigserial` columns draw from a real `<table>_<column>_seq` sequence.
`INSERT … SELECT` writes the query's rows (it used to answer `INSERT 0 0`
with nothing written), literal column DEFAULTs are stored and applied, and
`cur.stream()` works because Describe now distinguishes a zero-field
RowDescription from NoData.

The `box` type landed with its `;` array delimiter — `'{(1,2),(3,4);(5,6),
(7,8)}'::box[]` parses and renders as PostgreSQL does, corners re-ordered
per coordinate — and `pg_type` reports `typdelim` per type. Doubles now
render as `float8out` (`1e+20`, `1e-07`, `Infinity`, `{1.5,2}`) instead of
Rust's shortest form (`1e20`, `inf`, `{1.5,2.0}`), a `float8` with a numeric
operand is float8 arithmetic, unary minus keeps a double's signed zero and
numeric has none. `user` / `current_user` / `session_user` / `current_role`
answer the role the client connected as, and an unaliased cast, `array[…]`,
`row(…)`, `coalesce`, `greatest` / `least` and `nullif` column is named as
PostgreSQL names it (`int4`, `float8`, `bpchar`, `array`, `row`, …) rather
than `?column?`. A cast PostgreSQL has no definition for (`5::box`,
`1.5::bool`, `true::int8`) is 42846 `cannot cast type …`, not a 22P02 parse
failure.

#### Fixed

- `crates/secantus-pgplan`: `parse_array` is a port of PostgreSQL's
  `array_in` — whitespace set, nested quoting, unquoted `NULL`, the
  `[lo:hi]=` prefix, per-element delimiter (`;` for `box`), and every 22P02
  message; `array_concat` follows `array_cat`; nested arrays typed by depth.
- `crates/secantus-pgserver`: binary array parameters with `ndim > 1` are
  reshaped into nested arrays; binary array results nest likewise.
- `crates/secantus-pgplan` / `-pgserver`: `INSERT … RETURNING` (named,
  computed, `*`), `executemany(..., returning=True)`, an empty bound list
  binds as an empty array; `serial` / `bigserial` sequences in
  `__sql_sequences__` (the Python server's shape), dropped with the table;
  `INSERT … SELECT`; literal column DEFAULTs in the catalog, applied to every
  omitted column; an expression DEFAULT is refused (0A000) instead of dropped.
- `crates/secantus-pgserver`: `SHOW` completes with a bare `SHOW` tag; a
  FROM-less `select … where <const>` honours the predicate (false / NULL is
  zero rows, a non-boolean is 42804, `'x'` is 22P02).
- `crates/secantus-pgplan`: `pg_sleep(seconds)` — a `void` (2278) column,
  waits on the connection thread; `copy (select <expr> …) to stdout`
  evaluates its expressions; a crossed range inside an array literal is
  22000 from the range parser.
- `crates/secantus-pgplan`: expressions over `generate_series` rows
  (arithmetic, casts, calls, date arithmetic, comparisons) evaluate per row
  and are named / typed as PostgreSQL does.
- `crates/secantus-pgserver` / `crates/vendor/pgwire`: Describe answers
  NoData only for statements that return no rows; a zero-field
  RowDescription (`select`) yields one empty row, so `cur.stream()` works.
- `crates/secantus-pgplan/src/geo.rs` (new): the `box` type — `parse_box`
  (every input spelling, per-coordinate corner ordering, NaN to the high
  corner, `"1e400" is out of range for type double precision` 22003),
  `box_text`, `float8_text` (PostgreSQL's `float8out`); `pgtypes::typdelim`.
  `pg_type` rows carry the real `typdelim`; oid 603 / 1020; a bound text
  parameter of oid 603 casts.
- `crates/secantus-pgplan`: `float8` text rendering is `float8out`
  everywhere (column, `::text`, `float8[]`); a `float8` with a numeric
  operand is float8 arithmetic (`0.1::float8 + 0.2`); unary minus negates a
  double outright (`-(0.0::float8)` is `-0`); numeric has no negative zero
  (`- 0.0` is `0.0`) and a zero mantissa with an exponent renders `0`
  (`0.00e3`).
- `crates/secantus-pgplan`: `user` / `current_user` / `session_user` /
  `current_role` are the connecting role (`name`, oid 19); `current_catalog`
  / `current_schema`; `"user"` is an ordinary missing column (42703).
- `crates/secantus-pgplan`: FROM-less select columns are named as
  PostgreSQL's `FigureColname`: a cast after its target type (`int4`,
  `float8`, `bpchar`, `numeric`, `char`, `box`), a nested cast after the
  outer type, `array` / `row` / `coalesce` / `greatest` / `least` / `nullif`
  after the keyword, and a cast of any of those keeps the inner name.
- `crates/secantus-pgplan`: `Error::CannotCoerce` (42846) for casts
  PostgreSQL does not define — `5::box`, `box::float8`, `1.5::bool`,
  `1::int8::bool`, `1::int2::bool`, `'2021-01-01'::date::bool`, `true::int8`,
  `true::int2`; `true::int` is `1`. The cast site's source type decides, so
  `'2021-01-01'::bool` stays the 22P02 text failure.

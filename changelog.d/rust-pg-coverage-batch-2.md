### The Rust PostgreSQL server: json's own text, encodings, and 52 more built-in functions

Taking a value out of a `json` document now answers the document's own text
for it, as PostgreSQL does: `'{"a": [1,  2]}'::json -> 'a'` is `[1,  2]`, and
a key given twice finds its last value. Every `json` extraction answered a
re-rendering before, and `->` found the first of two equal keys.

A run of PostgreSQL 15's built-in function catalog against both servers went
from 1,451 functions answering the same to 1,503. Each fix is recorded with
PostgreSQL 15.19's answer in `pg_corpora/builtin_coverage.sql`, which CI
checks.

#### Fixed

- **`json` extraction** (`->`, `->>`, `#>`, `#>>`, `json_extract_path[_text]`,
  `json_each[_text]`, `json_array_elements[_text]`) answers the input's text
  for the value, and the last value of a key given twice. `jsonb_each` of a
  key given twice answered its first value.
- **`to_tsvector(json)`** no longer leaves a position empty after a value
  that was all stop words.
- **`convert_from(bytes, 'LATIN2')`**, and every LATIN encoding but LATIN1,
  read the bytes as LATIN1 (`\xb1` was `±`, not `ą`). An encoding with no
  table here converts its ASCII range and refuses anything above it.
- **`pg_catalog."char"(x)`** is the one-byte type, not `character`.
- **`make_timestamptz` with an unknown zone** and **`substring(s, p, e)`
  with an escape longer than one character** are refused, as PostgreSQL
  refuses them.
- **`json[b]_each` in a select list** answers a record where it was an
  internal error (XX000), and `LIMIT` / `OFFSET` over a select-list
  set-returning function cut its rows.
- **A one-column function in FROM keeps its OUT parameter's name**:
  `json_array_elements` is `value`.

#### Added

- Every encoding name PostgreSQL accepts (`pg_char_to_encoding('abc')` is
  WIN1258), and `to_ascii` with PostgreSQL's tables for LATIN1, LATIN2,
  LATIN9 and WIN1250.
- `unnest` of a `tsvector` and of a multirange; `setweight` and `ts_filter`
  with `"char"` arguments; `ts_token_type`; `json[b]_to_tsvector` with a
  filter.
- `json[b]_populate_record[set]` over a composite type in FROM, in a select
  list and inside an expression, with the base record's fields for missing
  keys.
- `range_merge` of a multirange and of two ranges, `int4range_canonical`
  and the other `*_canonical` and `*_subdiff` range functions.
- `like_escape`, `inet::cidr`, `interval::time`, `pg_lsn(numeric)`,
  `timestamp(date, time)` and `timestamptz(date, time[tz])`,
  `pg_get_keywords`, `pg_options_to_table`, `session_user()`,
  `generate_subscripts(a, d, reverse)`, `mxid_age`, `int8_sum`, and
  `age(xid)` of the special transaction ids.
- `count()` with no argument is PostgreSQL's 42809.

#### Still open

`tasks/backlog-rust-postgres.md` has the rest: `age(xid)` of a normal transaction id,
`ts_parse` / `ts_debug` / `ts_rewrite`, the `tid` type, the ACL and XML
functions, and a set-returning function inside an expression.

### The Rust PostgreSQL server gains full-text search, formatting, datetime functions, SQL/JSON paths and the statistical aggregates

Full-text search now works on the Rust PostgreSQL server: `to_tsvector`,
`to_tsquery` and its `plainto_` / `phraseto_` / `websearch_` forms, `@@`,
phrase operators, weights, `ts_rank` / `ts_rank_cd`, `ts_headline` and the
tsvector editing functions. It follows PostgreSQL 14's `english` configuration
(Snowball stemmer, stop list, parser token types) and its `simple` one. A
tsvector column written by either server can be read and searched by the
other.

`to_char` / `to_number` for numbers and `to_char` / `to_date` /
`to_timestamp` for dates and times are transcribed from PostgreSQL's
`formatting.c`, and so is the rest of the datetime family: `extract` /
`date_part`, `date_trunc`, `age`, `justify_*`, `make_*`, `date_bin`,
`isfinite`, BC dates, and `time` / `timestamp` arithmetic.

SQL/JSON path is implemented: the `jsonpath` type, `jsonb_path_query` (as a
set-returning function), `jsonb_path_query_array` / `_first` / `_exists` /
`_match`, `@?` and `@@`, in lax and strict modes, with `vars` and `silent`.

#### Added

- Statistical aggregates: `var_*` / `stddev_*` / `variance` / `stddev`
  (exact over `numeric`), `corr`, `covar_*`, every `regr_*`, and `bit_and` /
  `bit_or`.
- Ordered-set and hypothetical-set aggregates: `percentile_cont` /
  `percentile_disc` (scalar and array), `mode()`, and `rank` / `dense_rank` /
  `percent_rank` / `cume_dist` `WITHIN GROUP`.
- `json_agg` / `jsonb_agg` / `json[b]_object_agg`, `to_json[b]`,
  `json[b]_build_object` / `_build_array`, `json_object`, `array_to_json`,
  `row_to_json`, `json[b]_strip_nulls`, with PostgreSQL's per-function
  spacing.
- A whole-row reference (`SELECT t FROM t`, `row_to_json(t)`) and named
  record fields.
- `char(n)` semantics: values pad on output (including GROUP BY keys and
  record fields) and compare blank-insensitively. An assignment that is too
  long is `22001`, unless the excess is blanks, which are dropped.
- `IN` / `NOT IN` and `SIMILAR TO` over constants; a constant `WHERE`
  (`true` / `false` / `NULL` / a parameter).

#### Fixed

- The Python PostgreSQL server reads a tsvector written by the Rust server,
  and treats a string opposite a tsquery in `@@` as a tsvector.

### Rust PostgreSQL server: built-in function coverage, typed values, text search parsing and window order

A large pass over what PostgreSQL 15's own catalogs and parser say,
measured against PostgreSQL 15.19 and pinned by new corpora
(`tid_type`, `ts_parser_funcs`, `srf_in_select_list`, `srf_placement`,
`unknown_args`, `typed_value_render`, `char_casts`, `window_output_order`,
`insert_arity`).

#### Added

- The `tid` type: input, output, the binary format, comparisons,
  `min` / `max`, `tidlarger` / `tidsmaller`, `tid[]`, COPY.
- `ts_parse`, `ts_token_type`, `ts_debug` and `ts_rewrite`, on a
  transcription of PostgreSQL's default text-search parser
  (`wparser_def.c`), which `to_tsvector` / `plainto_tsquery` now use too.
- The single-byte encodings (LATIN2..LATIN10, ISO_8859_5..8, WIN866,
  WIN874, WIN1250..WIN1258, KOI8R, KOI8U) in `convert`, `convert_to`,
  `convert_from` and as a `client_encoding`, from tables measured on
  PostgreSQL, with its direct conversions between them.
- Set-returning functions in ORDER BY, GROUP BY and DISTINCT, nested in
  another's argument, and record-returning ones in expressions
  (`(json_each(j)).key`).
- The text pattern operators `~<~` `~<=~` `~>=~` `~>~`, `date + timetz`,
  `date_part` / `extract` over a `timetz`, and the pseudo-types' output
  functions.

#### Fixed

- With a single-byte `client_encoding` other than LATIN1 / LATIN9, text
  from the client was read as UTF-8 and stored as replacement characters.
- `'1e400'::float8` was Infinity and `'1e-400'::float8` 0; both are 22003.
- A set-returning function inside CASE, COALESCE, an aggregate, FILTER,
  WHERE, HAVING, JOIN ON, LIMIT / OFFSET, VALUES or UPDATE SET is refused
  as PostgreSQL refuses it; three of those answered rows.
- `insert into t values (1, 'x'), (2, 'y', 3)` succeeded; VALUES lists of
  different lengths are 42601. INSERT arity errors carry their position.
  Fixes RP-021
  (tests/backlog/test_rust_postgres_gaps.py::test_insert_arity_error_carries_position).
- Values print by their type in `||`, `concat`, `format`, `ROW`, the JSON
  functions and arrays (a `pg_lsn` printed as its number, `money` lost its
  `$`); `greatest` / `least` / CASE / COALESCE / NULLIF and UNION /
  INTERSECT / EXCEPT give an untyped literal the other side's type; COPY
  FROM reads every type through its input function.
- Casts and operators PostgreSQL 15's `pg_cast` / `pg_operator` have no
  entry for are refused (42846 / 42883) instead of answering;
  `"char"` casts and `"char"()` calls follow PostgreSQL separately.
- jsonb casts to a number or boolean say 22023 `cannot cast jsonb object
  to type integer`; date/time unit errors use PostgreSQL 15's wording.
- A built-in given untyped literals takes the overload PostgreSQL picks
  (`abs('-1')`), or 42725 where it cannot choose.
- A syntax error points at the token PostgreSQL points at.
- An uncorrelated subquery's value keeps its column type. Fixes RP-023
  (tests/backlog/test_rust_postgres_gaps.py::test_case_over_a_scalar_subquery_keeps_its_type).
- An untyped literal under `COLLATE` compares as the other side's type.
  Fixes RP-022
  (tests/backlog/test_rust_postgres_gaps.py::test_collated_literal_against_a_char_column).
- A query with window functions and no ORDER BY returns rows in the order
  PostgreSQL's WindowAgg leaves them. Fixes RP-024
  (tests/backlog/test_rust_postgres_gaps.py::test_window_rows_come_out_in_window_sort_order).
- A table may be named after a built-in type (`create table tid (...)`).
- Part of RP-015, which stays open for what is left
  (tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed).

### Rust PostgreSQL server: built-in functions, checked one by one

A new probe calls every immutable or stable function in PostgreSQL 15's
`pg_proc` on PostgreSQL and on the Rust PostgreSQL server, with one sample
value for each argument type. Of the 1,935 functions it can call, COV_BEFORE
answered the same before this change and COV_AFTER answer the same after it.
The corpora written by hand had not reached most of what it found.

#### Fixed

- `sum(interval)` answered zero and `avg(interval)` answered NULL, over a
  table and over literals. Both add the months, days and time apart, as
  PostgreSQL does.
- `substr`, `substring`, `overlay`, `md5`, `btrim`, `ltrim`, `rtrim` and
  `trim` over a `bytea` ran the text function over a rendering of the
  bytes, or were refused. They are the `bytea` functions now.
- `mod(7.5, 2)` answered 1 and `div(9.9, 0.3)` was a division by zero: both
  truncated their arguments to integers first.
- `smallint` arithmetic answered `integer`, and never overflowed. It
  answers `smallint`, and a result past its range is 22003. A prefix `-` or
  `abs` of a `real` or a `smallint` keeps the type too.
- `to_tsvector` over `json` or `jsonb` indexed the document's text, keys
  and numbers included. It indexes the string values only.
- `array_agg` over an array column was typed `varchar`; it is the array's
  own type.
- `diagonal(box)` and a `box` cast to `lseg` ran from the lower left corner
  to the upper right. PostgreSQL's runs the other way.
- A regular-expression function took any letter as an option. One that is
  no option is 22023 `invalid regular expression option`, for
  `regexp_like`, `regexp_count`, `regexp_instr`, `regexp_substr`,
  `regexp_replace`, `regexp_matches`, `regexp_split_to_array` and
  `regexp_split_to_table`.
- `pg_size_bytes` of a bad size is 22023 and `ts_rank` with a short weight
  array is 2202E, where both were other codes. A bad range bound flag,
  `is_normalized` with an unknown form and `json_array_elements` of a
  non-array carry PostgreSQL's text.

#### Added

- `regexp_match`.
- `regexp_replace` with a start position, and with the number of the match
  to replace.
- `OVERLAPS`, over dates, times and timestamps, with an end or a length.
- `bit_xor`; `bit_and` and `bit_or` over bit strings; `string_agg` over
  `bytea`.
- `date + time`.
- `get_bit`, `set_bit` and `bit_count` over `bytea`, and
  `length(bytea, encoding)`.
- `num_nulls` and `num_nonnulls`, `parse_ident`, `convert_to` and
  `convert` (UTF8, LATIN1 and SQL_ASCII), `multirange(range)`,
  `trunc(macaddr)`, `trunc(macaddr8)` and `macaddr8_set7bit`.
- A built-in called by the name of the function behind an operator:
  `int4pl(a, b)` is `a + b`, `texteq`, `int48lt`, `textlike` and about 750
  more, from PostgreSQL 15's `pg_operator`. Also the `*larger` /
  `*smaller` functions behind `max` / `min`, the numeric and date `*cmp`
  functions, and names such as `textlen`, `dexp` and `numeric_sqrt`.
- The embedded Python handle, `_secantus_server.PgServer`, takes
  `tls_cert_file`, `tls_key_file`, `tls_ca_file`, `tls_client_cert`,
  `tls_require`, `refuse_unknown_users`, `hba_file` and `ident_file`, and
  has `reload()`. It is a tool for this repository's tests; the wheel on
  PyPI does not carry it.
- `./inv validate-psycopg-rust [--tls]` runs psycopg's suite against the
  Rust PostgreSQL server, and with `--tls` again over TLS. A new workflow
  runs both on Linux each week and on a pull request that touches the
  server's TLS code. It fails when a run stops short, or when a test fails
  over TLS that passes in the clear.

#### Changed

- A statement that passed an unknown letter as a regular-expression option
  ran before and is refused now, as on PostgreSQL.
- `smallint` arithmetic that overflowed gave a number before and is an
  error now.

`tools/probes/pg_builtin_coverage.py` is the probe.
`tools/probes/pg_corpora/builtin_coverage.sql` holds 135 statements over
what changed, with PostgreSQL 15.19's answers recorded beside it, and
`tests/test_rust_pg_builtin_coverage.py` holds the server to them in CI.
What the probe still reports is in `tasks/backlog.md`.

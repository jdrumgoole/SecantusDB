### Rust PostgreSQL server: backlog batch 4

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- A table's row type follows `ALTER TABLE`: after `add column`,
  `'(1,x)'::t` had been 22P02 `malformed record literal`, and after
  `drop column` a literal was read with the dropped column's type, because
  the row type kept the shape the table was created with
  (`tests/backlog/test_rust_postgres_gaps.py::test_row_type_follows_alter_table`).
- A text literal or a record cast to a table's row type, or to a
  `CREATE TYPE ... AS` composite, pads a `char(n)` field to its width
  (`("ab ",x)`) and refuses one too long (22001), as the column does. The
  width is recorded in the type's catalog document (`widths`), beside the
  fields the Python server reads. Part of RP-032
  (`::test_record_literal_pads_char_field`).

#### Added

- `pg_timezone_names` and `pg_timezone_abbrevs`, as the views and as the
  functions in FROM: PostgreSQL 15.19's 598 zone names, each with its
  abbreviation, offset and DST flag at this moment, and its 195 `Default`
  abbreviations. A client listing time zones had got 42P01. The zone
  rules are `chrono-tz`'s, so a zone whose rules its tz database and
  PostgreSQL's disagree on answers by `chrono-tz`'s (Morocco's two, on
  2026-10-15). Part of RP-015
  (`tests/backlog/test_rust_postgres_gaps.py::test_builtin_coverage_fixed[timezone-names]`,
  `[timezone-abbrevs]`).
- Corpora `timezone_views` and `row_type_fields`.

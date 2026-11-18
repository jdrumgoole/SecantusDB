### A bpchar UNIQUE is keyed with its trailing blanks ignored

A UNIQUE constraint or unique index over an unsized `bpchar` column now
compares keys the way PostgreSQL's `bpcharcmp` does, ignoring trailing
blanks, through a storage unique index over a hidden field holding the
trimmed value. A value COPY loaded as `'w  '` no longer slips past a later
`'w'`, and building a unique index over rows that differ only in trailing
blanks is refused with PostgreSQL's `could not create unique index`, naming
the row stored first, instead of leaving a broken index behind.

#### Fixed

- Rust PG server: a `bpchar` UNIQUE constraint or unique index (single or
  multi-column, CREATE TABLE, ALTER TABLE ADD UNIQUE, CREATE UNIQUE INDEX)
  is keyed by the value with trailing blanks trimmed; the catalogs still
  show a plain column index. Step 2 of RP-693
  (tests/test_rust_pg_bpchar_key.py::test_a_bpchar_unique_constraint_ignores_trailing_blanks,
  tests/test_rust_pg_bpchar_key.py::test_a_bpchar_unique_over_rows_differing_only_in_blanks_is_refused).

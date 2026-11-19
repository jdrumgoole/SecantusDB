### ON CONFLICT finds a bpchar UNIQUE's row whatever its trailing blanks

`INSERT ... ON CONFLICT` over a UNIQUE constraint or unique index on an
unsized `bpchar` column now arbitrates on the key as PostgreSQL compares it,
trailing blanks ignored. Inserting `'w'` over a stored `'w  '` updates (or
skips) that row instead of failing with a duplicate-key error, and a column
list naming a `bpchar` unique index -- partial or not -- is recognised as
its arbiter instead of being refused with "there is no unique or exclusion
constraint matching the ON CONFLICT specification".

#### Fixed

- Rust PG server: ON CONFLICT over a `bpchar` UNIQUE (a column list, `ON
  CONSTRAINT`, or an inferred unique index) arbitrates on the trimmed key.
  Step 3 of RP-693
  (tests/test_rust_pg_bpchar_key.py::test_on_conflict_on_a_bpchar_unique_finds_a_row_kept_with_blanks,
  tests/test_rust_pg_bpchar_key.py::test_on_conflict_infers_a_bpchar_unique_index).

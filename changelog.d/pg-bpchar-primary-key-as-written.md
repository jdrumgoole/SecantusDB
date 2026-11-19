### A bpchar primary key keeps its value as written

PostgreSQL compares `bpchar` values without their trailing blanks, so `'k'`
and `'k  '` are the same primary key, but an unsized `bpchar` column still
returns the value as it was written, blanks and all. The Rust PostgreSQL
server got the key right and lost the blanks: `octet_length(a)` of a stored
`'k  '` was 1, not 3. A table created now keeps the value as written in its
own field and keys the row by the trimmed value, so equality, ON CONFLICT,
UPDATE, DELETE and foreign keys still treat `'k'` and `'k  '` as one key,
and the row reads back as it was written. A table created before this change
keeps its old layout and reads back as it always has.

The same work fixed a COPY bug: a row copied in as `'w  '` was keyed with its
blanks, so a later `INSERT` of `'w'` did not collide. That affected `char(n)`
keys, composite keys and keys added by `ALTER TABLE`. PostgreSQL 15 refuses
the second row with 23505, and so does this server now.

#### Fixed

- `secantus-pgcatalog` / `secantus-pgplan` / `secantus-pg`: a single-column
  unsized `bpchar` PRIMARY KEY made by CREATE TABLE stores the value as
  written under the column's name and derives `_id` from it with trailing
  blanks trimmed (`bpchareq` / `bcTruelen`) on INSERT, INSERT ... SELECT,
  UPDATE (BEFORE triggers included) and COPY. ON CONFLICT and foreign keys
  match on the trimmed key. This is an on-disk layout change for those
  tables only; the MongoDB server shares the storage format but never writes
  a `bpchar` key. Step 1 of RP-693
  (tests/test_rust_pg_bpchar_key.py::test_a_bpchar_primary_key_keeps_the_value_as_written).
- COPY into a table with a `bpchar` key (`char(n)`, composite, or an
  ALTER-added unsized key) trims the key, so a duplicate that differs only in
  trailing blanks is 23505
  (tests/test_rust_pg_bpchar_key.py::test_copy_trims_every_bpchar_key).

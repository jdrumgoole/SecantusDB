### A column DEFAULT is held to its column's length

A constant DEFAULT is now coerced to its column's type and modifier when an
INSERT fills the column with it, as PostgreSQL does: a `varchar(2)` column
whose default is `'abc'` refuses the row with 22001 `value too long for
type character varying(2)` instead of storing three characters.

#### Fixed

- A folded DEFAULT went into the row without the length check a written
  value gets, so an overlong `char(n)` / `varchar(n)` default was stored.
  Fixes RP-892 (tests/backlog/test_rp_found_rp886.py::test_a_folded_default_is_coerced_to_its_column)

### A `cstring` value casts only to the string types

On PostgreSQL the `cstring` pseudo-type has no casts but the I/O conversions
to the string types, so `'12'::cstring::int4` is refused with 42846 `cannot
cast type cstring to integer`. The Rust PostgreSQL server read the text as
the target type instead; it now refuses the cast, at the cast's position.

#### Fixed

- `cstring` to a non-string type is 42846, as on PostgreSQL 15.19.
  Fixes RP-1041 (tests/backlog/test_rp_found_rp750.py::test_cstring_casts_only_to_strings)

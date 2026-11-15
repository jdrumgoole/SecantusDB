### The record_image operators compare records by their binary images

`*=`, `*<>`, `*<`, `*<=`, `*>` and `*>=` now work over two record values, as
PostgreSQL's `record_image_eq` .. `record_image_ge` do: each field pair is
compared by its stored image, so `(select row(1.0)) *= (select row(1.00))` is
false where `=` is true, a negative integer sorts after a positive one, and
NULLs sort last. Between two `ROW(...)` constructors the operator is each
pair's, so `row(1) *= row(1)` is 42883 `operator does not exist: integer *=
integer`.

#### Fixed

- Rust PG server: the `record_image` operators were refused with 0A000.
  Fields of int2, int4, int8, oid, bool, float4, float8, numeric (its short
  and long on-disk layouts), text, varchar, bpchar and bytea are compared;
  other field types are refused by name (RP-976).

Fixes RP-650 (tests/backlog/test_rp_found_rp590.py::test_record_image_operators)

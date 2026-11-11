### Rust PG server: a column of a domain over a domain is typed by the inner domain in `information_schema.columns`

A column whose type is a domain over another domain (`create domain d2 as
d1`) is now listed by `information_schema.columns` as PostgreSQL lists it:
`data_type` is `USER-DEFINED`, `udt_schema` / `udt_name` name the inner
domain, and the length, precision and datetime columns are NULL. The view
reads the domain's immediate base type (`typbasetype`), which here is a
domain itself.

#### Fixed

- Such a column reported the ultimate base's type: `integer` / `int4` with
  `numeric_precision` 32 for a domain over a domain over `int`, `character`
  / `bpchar` with `character_octet_length` 1073741824 for one over a
  `char(3)` domain, `ARRAY` / `_int4` for one over an `int[]` domain, and a
  `datetime_precision` of 6 for one over a `timestamp(3)` domain. A column
  of a domain over a built-in type is unchanged. This holds for a column
  CREATE TABLE declares and one ADD COLUMN adds.
- Fixes RP-827 (tests/backlog/test_rp_found_rp799.py::test_data_type_of_a_domain_over_a_domain_is_user_defined),
  with the regression test
  tests/backlog/test_rp_found_rp799.py::test_a_domain_over_a_domain_column_is_typed_by_the_inner_domain.

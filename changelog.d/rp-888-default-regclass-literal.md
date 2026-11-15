### A sequence default names a sequence that exists

`CREATE TABLE t (a int default nextval('nosuch_seq'))` is now refused at the
CREATE with 42P01 `relation "nosuch_seq" does not exist`, at the literal, as
PostgreSQL refuses it: analysing the default coerces the literal to
`nextval`'s `regclass` parameter, which looks the relation up. Before, the
CREATE succeeded and the first INSERT failed.

#### Fixed

- A literal passed to `nextval` / `currval` / `setval` in a DEFAULT is
  resolved as `regclass` when the default is declared.
  Fixes RP-888 (tests/backlog/test_rp_found_rp881.py::test_a_regclass_literal_in_a_default_is_resolved)

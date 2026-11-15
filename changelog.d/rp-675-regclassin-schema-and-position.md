### A regclass literal naming a missing schema says so, at the literal

PostgreSQL's `regclassin` looks a qualified name's schema up before the
relation in it, so `'nosuch.s'::regclass` and `pg_relation_size('nosuch.s')`
report `schema "nosuch" does not exist` (3F000), not a missing relation. The
Rust PostgreSQL server now does the same, and `to_regclass` still answers
NULL for a missing schema. A failed lookup of a literal read during analysis
is also placed at the literal however the name inside it is written, so
`'"Nosuch"'::regclass` and `nextval($$nosuchseq$$)` carry the position
PostgreSQL gives them.

#### Fixed

- `regclass` input over a missing schema is 3F000; a regclass literal's
  42P01 points at the literal, quoted or dollar-quoted.
  Fixes RP-675 (tests/backlog/test_rp_found_rp458.py::test_regclassin_errors)

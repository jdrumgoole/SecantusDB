### Rust PG server: an untyped literal read as `bpchar` keeps its trailing blanks

In a VALUES list or a set operation beside a `char(n)` member, an untyped
string literal takes the column's `bpchar` type with no modifier, and
PostgreSQL keeps it as written: `select 'a'::char(3) union all select 'b '`
is `b `, 2 bytes, on 15.19. The Rust PostgreSQL server read the literal
through the cast that stores a `char(n)` value trimmed, so it answered `b`.

#### Fixed

- An untyped literal resolved to `bpchar` by a VALUES column or a set
  operation keeps its trailing blanks, on either side and under a filter
  that compares it as `bpchar`.

Fixes RP-804 (tests/backlog/test_rp_found_rp794.py::test_an_untyped_literal_resolved_to_bpchar_keeps_its_blanks)

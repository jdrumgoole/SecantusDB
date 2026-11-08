### Rust PG server: an array of a domain is the domain's array type

`select array['a b'::d]`, where `d` is a domain over `char(3)`, went out
described as `varchar` (oid 1043) with the text `{a b}`. PostgreSQL 15.19
describes it as the domain's own array type, the `typarray` that `CREATE
DOMAIN` makes beside the domain, and prints `{"a b"}`: `array_out` quotes
an element that holds a blank, through the base type's output function. A
mix of a domain with another type was worse: `array[1::d, 2]` over a domain
of `int` was described as an `int4` holding the text `{1,2}`, which psycopg
could not read.

#### Fixed

- An array of a domain is described with the domain's `typarray` oid. Its
  text and binary forms are the base type's array (`array_out` /
  `array_send`), and in binary the header carries the domain as the element
  type, as PostgreSQL's does.
- An ARRAY constructor keeps a domain only when every element is of it, as
  `select_common_type` does. Mixed with another type, or with an untyped
  NULL, string or parameter (`unknown`), each domain falls back to its base
  type: `array[1::d, 2]` and `array[null, 1::d]` are `int4[]`, and
  `array['a b'::d, 'c'::char(3)]` is `bpchar[]`.

Fixes RP-559 (tests/backlog/test_rp_found_rp551.py::test_domain_array),
with the regression tests
`tests/backlog/test_rp_found_rp551.py::test_domain_array_common_type` and
`::test_domain_array_binary`, and the crate test
`tests::an_array_of_one_domain_is_the_domains_array`. Files RP-569 (an
array built from a domain column is typed by the column's base type) and
RP-570 (a cast to a domain's array does not apply the base type's modifier).

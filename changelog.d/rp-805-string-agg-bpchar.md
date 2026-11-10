### Rust PG server: `string_agg` over an unsized `bpchar` column drops its trailing blanks

`string_agg(text, text)` takes a `bpchar` argument through the implicit
`bpchar` -> `text` cast, which drops the trailing blanks. Over a column with
no width -- a VALUES list or a set operation whose `char(n)` members differ in
width -- the Rust PostgreSQL server joined the cells with their blanks:
`select string_agg(f, '|') from (values ('a  '::char(3)), ('b'::char(2)))
s(f)` answered `a  |b ` where PostgreSQL 15.19 answers `a|b`.

#### Fixed

- `string_agg` over a `bpchar` argument trims each value's trailing blanks
  (a leading blank and a tab stay) before DISTINCT dedups, in an aggregate
  and as a window function alike.

Fixes RP-805 (tests/backlog/test_rp_found_rp794.py::test_string_agg_over_an_unsized_bpchar_drops_its_blanks)

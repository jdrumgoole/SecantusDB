### Rust PG server: a `char(n)` column compared with a `text` value compares as `text`, and IN / BETWEEN drop a `char(n)` constant's blanks

`select k from t where f = 'xy  '::text` over a `char(3)` column now finds
no row, as on PostgreSQL 15.19. There is no `bpchar = text` operator, so
PostgreSQL casts the column to `text` (`rtrim`) and compares `text`s, where
the constant's blanks count. The WHERE lowering dropped them as `bpchar`'s
operator would, so it found the `'xy'` row. A `name` constant compares the
same way, and `<`, `<>`, a `||` or `rpad(...)` result follow, while an
untyped, `char(n)` or `varchar` constant still compares as `bpchar`. Working
it found that an IN list and a BETWEEN over a `char(n)` column did not drop
the blanks at all: `f in ('xy  ')` and `f between 'xy  ' and 'xy  '` found
nothing, and `f not in ('xy  ', 'q')` kept the `'xy'` row. An IN list's
members take the column's type, so `in ('xy  '::text)` compares as
`bpchar` too; each BETWEEN bound is its own operator.

#### Fixed

- A `char(n)` column compared in a WHERE with a `text` or `name` value
  compares as `text`, keeping the constant's trailing blanks.
- An IN list and a BETWEEN over a `char(n)` column compare as `bpchar`,
  dropping the constants' trailing blanks.

Fixes RP-685 (tests/backlog/test_rp_found_rp680.py::test_char_n_against_text_compares_as_text)

Fixes RP-691 (tests/backlog/test_rp_found_rp680.py::test_in_and_between_over_char_n_compare_as_bpchar),
filed and fixed while working RP-685.

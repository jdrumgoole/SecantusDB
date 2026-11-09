### Rust PG server: a `char(n)[]` column's elements are padded to the width, and compared without their blanks

A `char(3)[]` column gave its elements back unpadded. `insert into t (c)
values (array['ab']) returning c, octet_length(c[1])` answered `{ab}`, 2,
where PostgreSQL 15.19 answers `{"ab "}`, 3. `c::text` printed `{ab}`.
Separately, a `char(n)[]` value that did carry its padding (from an explicit
`::char(3)[]` cast or from `array_agg` over a `char(n)` column) was compared
with its blanks: `'{ab}'::char(3)[] = '{ab}'`, `@> '{ab}'`, `&& '{ab}'`,
`IN`, `array_position(..., 'ab')` and `array_remove(..., 'ab')` each missed
the element, and `WHERE c = '{ab}'::char(3)[]` over a column found nothing.

PostgreSQL keeps each element of a `char(n)[]` padded and compares the
elements with `bpchareq` / `bpcharcmp`, which ignore trailing blanks. This
server now treats the elements the way it treats a scalar `char(n)`: stored
trimmed, padded on output and where they enter an expression, and compared
trimmed. So `c[1]` is `ab `, `array_to_string(c, ',')` and `to_json(c)`
keep the blanks, a cast to `text[]` drops them (`rtrim1`), and
`=`, `<>`, `<`, `@>`, `<@`, `&&`, `IN`, `array_position(s)`, `array_remove`
and `array_replace` ignore them. Storage keys and one-table filters are
unchanged, as the stored elements were already trimmed.

#### Fixed

- A `char(n)[]` column's elements print padded to the width, in a plain
  select, a `RETURNING`, a `::text` cast and over a join.
- Comparisons and searches over a `char(n)[]` value ignore each element's
  trailing blanks.

Fixes RP-719 (tests/backlog/test_rp_found_rp710.py::test_a_char_n_array_element_is_padded_to_its_width,
tests/backlog/test_rp_found_rp719.py::test_a_char_n_array_is_padded_and_compared_without_its_blanks)

#### Filed

RP-720 to RP-725, found while working it: `&&` with an untyped array
literal is 42883; `&&` misses an array with its own lower bound; GROUP BY
over an array column ignores the ORDER BY; in a join's WHERE an untyped
array literal, and a padded `char(n)` constant, match nothing; and GROUP BY
splits a padded `char(n)` value from the same value unpadded.

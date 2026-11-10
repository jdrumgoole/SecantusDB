### Rust PG server: a WHERE over a VALUES list's `char(n)` column compares as `bpchar`

A VALUES list or a set operation whose members' `char(n)` modifiers differ
gives an unsized `bpchar` column, whose values keep their trailing blanks
as written (`'uu  '`). The Rust PostgreSQL server kept them that way, but
its WHERE filter compared the blanks as it found them: `f = 'uu'` and
`f in ('uu  ', 'tt')` missed the `uu  ` row, and `f = 'uu  '::text` found
it, where PostgreSQL finds it with the first two and not the third. Such a
predicate is now evaluated per row, by the operators `bpchar` resolves to.

#### Fixed

- Over an unsized `bpchar` column of a VALUES list, a set operation, or a
  CTE over either, `=`, `<>`, the orderings, `BETWEEN`, `IN` / `NOT IN`
  and `IS [NOT] DISTINCT FROM` in a WHERE ignore trailing blanks against
  a `bpchar` or untyped value, and against a `text` one compare the column
  `rtrim`med, as PostgreSQL 15.19 does (a `text` member of a longer `IN`
  list, and a scalar subquery's `text` value, aside: RP-792).

Fixes RP-775 (tests/backlog/test_rp_found_rp757.py::test_a_values_char_column_compares_as_bpchar_in_a_where).
Files RP-792, RP-793 and RP-794, found while working it.

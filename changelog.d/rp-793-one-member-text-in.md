### Rust PG server: a one-member IN list of `text` over a `char(n)` column compares as `text`

PostgreSQL's `transformAExprIn` turns an IN list into one array comparison,
over the list's common type, only when the list has more than one non-Var
member. A single member is a plain `=`, resolved like any other comparison.
This server used to coerce every member to the column's type, so a lone
`text` member compared as `bpchar` and lost its trailing blanks.

#### Fixed

- Over a `char(4)` column holding `uu`, `where f in ('uu  '::text)` finds
  nothing, as on PostgreSQL 15.19. The comparison is `texteq` over the
  `rtrim`med column. Before this fix it found `uu`, and `not in` dropped it.
  Longer lists, and a lone untyped member, still compare as `bpchar`.

Fixes RP-793 (tests/backlog/test_rp_found_rp775.py::test_a_one_member_text_in_list_compares_as_text)

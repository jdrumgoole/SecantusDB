### Rust PG server: a column alias list over a subquery's `*` names every column

`select t from (select * from a join b on a.i = b.i) q(p, r, s, t)` now
answers as PostgreSQL 15.19 does. It used to be refused with `table "q" has
1 columns available but 4 columns specified`, unless the query read only the
first column.

#### Fixed

- The step that replaces a FROM-subquery's unread outputs with NULL
  (PostgreSQL's `remove_unused_subquery_outputs`) matched the alias list to
  the select list by position, and counted a `*`, `x.*` or `(expr).*` target
  as one output. So `q(p, r, s, t)` named the star `p`, and when `p` was not
  read the whole star became one NULL column. A star target, and every
  target after it while an alias list is given, now keeps its outputs.

Fixes RP-773 (tests/backlog/test_rp_found_rp768.py::test_an_alias_list_over_a_joined_star)

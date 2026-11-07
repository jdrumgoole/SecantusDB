### Rust PG server: an EXISTS subquery's names are resolved before its select list is dropped

An EXISTS only asks whether its subquery returns a row, so its select list,
ORDER BY and DISTINCT ON are dropped before it runs, as PostgreSQL's
`simplify_EXISTS_query` drops them. PostgreSQL does that only after
`transformSubLink` has analysed the subquery whole, so a name the list
cannot resolve is still an error. Here the list was dropped unread. Now a
column reference the subquery's own tables do not answer is kept for the
planner, which resolves it as an outer row's column or refuses it as
PostgreSQL does. The one-table EXISTS that is rewritten to an IN steps
aside for such a name.

#### Fixed

- `select exists (select nosuch)`, `select 1 where exists (select nosuch)`,
  `exists (select nosuch from t)`, `exists (select 1 from t order by
  nosuch)`, `exists (select distinct on (nosuch) 1)` and `where exists
  (select nosuch from u where u.a = t.a)` are 42703 `column "nosuch" does
  not exist` at the name, as on PostgreSQL 15.19. Before, they answered
  `true` or a row.
- `exists (select a from t, u)`, with `a` in both tables, is 42702 `column
  reference "a" is ambiguous`.
- A name only an outer level has (`exists (select z from t)` over `(select
  1 z) o`) still answers, and `exists (select 1/0)` is still `true`.

Fixes RP-444 (tests/backlog/test_rp_fromless_analysis.py::test_exists_target_list_is_analysed, tests/backlog/test_rp_fromless_analysis.py::test_exists_target_list_is_analysed_over_tables)

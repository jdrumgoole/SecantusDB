### Rust PG server: a FROM-less subquery two levels down reads the outer row

`select (select (select a)) from (select 1 a) t` answers 1, as on
PostgreSQL 15.19. It was 42703 `column "a" does not exist`, or with a
qualified `t.a` a 42883 naming the server's internal `\x1fcorrelated` call.

#### Fixed

- A bare column name in a subquery is looked up in every enclosing query,
  nearest first, as PostgreSQL's `colNameToVar` does. It was looked up only
  in the FROM of the query right around the subquery, so a FROM-less
  subquery in between hid the outer row. The nearer level still wins, and a
  name no level has stays 42703.
- A FROM-less subquery whose target is itself a correlated subquery plans
  and runs. While the planner probes such a body for its type, the nested
  per-row call is a NULL of its type and runs nothing.

Fixes RP-445 (tests/backlog/test_rp_fromless_analysis.py::test_fromless_subquery_two_levels_down_reads_the_outer_row)

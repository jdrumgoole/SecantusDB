### Rust PG server: a GROUP BY constant that is no integer is 42601

A GROUP BY item written as a bare constant that is not an integer --
`group by 1.5`, `'x'`, `null`, `true`, `B'101'`, `2147483648` -- is now
PostgreSQL's 42601 `non-integer constant in GROUP BY` at the constant, as
`findTargetlistEntrySQL92` raises it.

#### Fixed

- Each was 0A000 `this target is not supported yet` with no position; a
  FROM-less `select 1 group by 'x'` and `select count(*) ... group by 1.5`
  returned rows; and `select a ... group by 1.5` was 42803. The check
  (`secantus-pgplan/src/group_sql92.rs`) runs on every select level a plan
  reaches, so a FROM subquery, a sublink, a CTE, a UNION branch and
  EXPLAIN are covered. It walks the list the way `flatten_grouping_sets`
  does, so a constant inside `rollup(...)`, `grouping sets (...)` or an
  implicit row `(1.5, 2)` counts, and `(1.5)` and `-1.5` are located where
  PostgreSQL locates them.
- What parse analysis meets before the GROUP BY still comes first: the
  statement is planned with its GROUP BY cut short before the constant, and
  an error from the select list, WHERE, HAVING, ORDER BY or an earlier
  GROUP BY item is the answer (`select nosuch ... group by 1.5` is 42703,
  `group by 9, 1.5` is 42P10). The ungrouped-column and GROUPING() checks,
  the locking clause and what a run raises come after it, as in
  PostgreSQL.
- Fixes RP-854 (tests/backlog/test_rp_found_rp700.py::test_a_non_integer_group_by_constant_is_42601,
  tests/backlog/test_rp_found_rp700.py::test_what_parse_analysis_meets_before_the_group_by_constant_comes_first),
  with a crate test (`secantus-pgplan` `a_non_integer_group_by_constant_is_42601`).
- Filed while working it: RP-871 (an ORDER BY constant that is no integer),
  RP-872 (the same in DISTINCT ON), RP-873 (a GROUP BY position inside a
  grouping set or a parenthesised list is grouped as a value) and RP-874 (a
  grouped query reports a column that does not exist as 42803), with gap
  tests in `tests/backlog/test_rp_found_rp854.py`.

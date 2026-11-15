### A GROUP BY or DISTINCT ON constant's error comes before a window definition's

PostgreSQL analyses a statement's WINDOW definitions after its GROUP BY and
DISTINCT ON clauses, so `group by 2.5 window w as (order by nosuch)` reports
the non-integer constant, not the missing column. The Rust PostgreSQL server
now does the same: the statement it plans to find the errors raised ahead of
the constant keeps each window's name but no longer analyses its definition.

#### Fixed

- Rust PG server: a non-integer GROUP BY, DISTINCT ON or ORDER BY constant
  is 42601 ahead of an error inside a WINDOW definition.
  Fixes RP-885 (tests/backlog/test_rp_found_rp872.py::test_a_window_definition_is_analysed_after_the_constant)

### Rust PG server: a DISTINCT ON constant that is no integer is 42601

A DISTINCT ON item written as a bare constant that is not an integer --
`distinct on (1.5)`, `('x')`, `(null)`, `(true)`, `(b'1')` -- is now
PostgreSQL's 42601 `non-integer constant in DISTINCT ON` at the constant,
instead of a key of one value that quietly returned the first row.

#### Fixed

- Each returned its row, or the error of a clause PostgreSQL analyses
  later. The check (`secantus-pgplan/src/group_sql92.rs`, beside the GROUP
  BY one) runs on every select level a plan reaches, so a FROM subquery, a
  sublink, a CTE and a UNION branch are covered, and `(1.5)` and `-1.5`
  are located where PostgreSQL locates them.
- It follows `transformSelectStmt`'s order: the select list, WHERE, ORDER
  BY, the GROUP BY (its own constant's 42601 included) and an earlier
  DISTINCT ON item come first; the ORDER BY match (`SELECT DISTINCT ON
  expressions must match initial ORDER BY expressions`), the
  ungrouped-column check, LIMIT, OFFSET and the locking clause come after.
  The GROUP BY check's cut statement now drops DISTINCT, which PostgreSQL
  analyses after GROUP BY.
- Fixes RP-872 (tests/backlog/test_rp_found_rp854.py::test_a_non_integer_distinct_on_constant_is_42601,
  tests/backlog/test_rp_found_rp854.py::test_what_comes_before_a_distinct_on_constant),
  with a crate test (`secantus-pgplan` `a_non_integer_distinct_on_constant_is_42601`).
- Filed while working it: RP-884 (a DISTINCT ON position past the select
  list returns rows) and RP-885 (a window definition's error comes ahead of
  a GROUP BY or DISTINCT ON constant's).

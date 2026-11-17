### Rust PostgreSQL server: an INTERSECT over an aggregate with a HAVING hashes the smaller input first

An aggregate input with a HAVING was not estimated, so an INTERSECT kept
it first even when the other input had fewer groups, and a group's row
came from the wrong side. PostgreSQL scales the aggregate's groups by the
HAVING's selectivity, which over an aggregate has no statistics and takes
the planner's defaults: a third for an inequality, 1/200 for an equality,
the complement for `<>` and `IS NOT NULL`, with NOT pushed into the
operator. The set operation's estimate now does the same.

#### Fixed

- `select max(x) from t692j group by k having max(x) > 0 intersect select
  1.000` returns `1.000`, as PostgreSQL 15.19 does, not `1.0`.
  Fixes RP-841, in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_having_input_puts_the_smaller_input_first)

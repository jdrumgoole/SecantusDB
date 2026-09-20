### The Rust PostgreSQL server supports HAVING

`select g, count(*) from t group by g having count(*) > 1` was refused
outright with `0A000 HAVING is not supported yet`, which ruled out most
grouped reporting queries.

HAVING now filters the groups after the aggregates are computed and before
ORDER BY, DISTINCT and LIMIT see them. An aggregate written only in HAVING is
computed for the test and never projected, so `group by g having count(s) = 0`
works without selecting that count. Comparisons match PostgreSQL's rules: a
NULL on either side makes the test UNKNOWN rather than true, numerics compare
by value, and a constant may be written on either side of the operator.

The accepted shape is deliberately narrow — a comparison or NULL test on an
aggregate or grouping key against a constant, combined with AND / OR / NOT.
Anything else is refused while planning, because HAVING decides which rows
come back and a half-understood predicate would answer wrongly.

#### Added

- `crates/secantus-pgplan`: a `Having` predicate on the aggregate plan, and
  the parser that builds it (appending any aggregate the SELECT list omits).
- `crates/secantus-pgserver`: `having_holds`, applied to the grouped rows.

#### Testing

- `tests/test_rust_pgserver_slice.py`: the connectives, NULL handling, an
  aggregate only HAVING asks for, a grouping key, a constant on the left, and
  HAVING with no GROUP BY — all measured against PostgreSQL 14.24.
- `crates/secantus-pgplan`: the plan reuses an existing aggregate item or adds
  one, and an unsupported shape is still refused.

### A missing column in an UPDATE ... FROM is reported before a later ambiguous one

An `UPDATE ... FROM` or `DELETE ... USING` checked every unqualified name for
ambiguity before resolving anything, so `update k set b = b from e where
e.nosuch = 1` answered 42702 for `b` where PostgreSQL 15.19 answers 42703 for
`e.nosuch`. The check now walks the statement in PostgreSQL's order (WHERE,
RETURNING, the SET list) and stops at the first bad column, missing or
ambiguous, pointing at it.

#### Fixed

- Rust PG server: `UPDATE ... FROM` / `DELETE ... USING` report the first bad
  column in PostgreSQL's analysis order.
  Fixes RP-436 (tests/backlog/test_rp_dml_from.py::test_missing_column_reported_before_ambiguous_one)

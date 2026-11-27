### Rust PG server: `EXPLAIN`, `PREPARE` and `CREATE VIEW` refuse a set operation's unmatched columns, positioned

`explain select null::text union select 1` and `prepare p as select
null::text union select 1` now fail with 42804 `UNION types text and integer
cannot be matched` at the `1`, as PostgreSQL 15 does in parse analysis. Both
used to succeed, because the check ran only when the set operation executed.
`create view v as` the same query was refused, but with no error position. It
now points at the column the client wrote.

#### Fixed

- A set operation's column types are checked from each side's description
  when a statement only analyses its query: `EXPLAIN` (with or without
  `ANALYZE`), `PREPARE` and a protocol-level Parse / Describe. This covers
  the column count (42601), the type categories (42804) and the conversion
  to the common type (42846). Nothing in the query runs for the check.
- `CREATE VIEW`'s parse-analysis errors carry the position PostgreSQL's
  `parser_errposition` gives them: a set operation's mismatched column, its
  column count, and a `CASE` / `COALESCE` whose branches do not match.
  Positions count in the client's text, also when the view is a later
  statement of a multi-statement query.

Fixes RP-1089 (tests/backlog/test_rp_found_rp1050.py::test_set_op_mismatch_refused_at_analysis, tests/test_rust_pg_set_op_positions.py::test_set_op_refused_at_analysis)

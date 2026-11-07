### Rust PG server: EXPLAIN refuses what parse analysis refuses

PostgreSQL analyses the statement under an EXPLAIN before it reads a single
option, so a statement it refuses is refused under every EXPLAIN form.
`explain select * from e where a = true` (an integer `a`) printed `Seq Scan
on e`, where PostgreSQL 15.19 answers 42883 `operator does not exist:
integer = boolean`, as this server already did for the SELECT alone. Worse,
`explain analyze delete from e where b = 1` ran the DELETE that the bare
statement refuses.

#### Fixed

- EXPLAIN, EXPLAIN ANALYZE and every EXPLAIN option list run the checks
  parse analysis makes before planning: an operator with no overload for its
  operands' types (42883), an untyped parameter or literal (42P18 / 42804)
  and an ambiguous ORDER BY name (42702). They run ahead of the options, so
  `explain (bogus) select * from e where a = true` is 42883, as in
  PostgreSQL; an option's error still comes before a planner-time one
  (`explain (bogus) select 1/0` is 42601).

Fixes RP-419 (tests/backlog/test_rp_where_types.py::test_explain_checks_operator_types)

#### Known gaps

- RP-427: an INSERT's SELECT and a WITH query are not type-checked, bare or
  under EXPLAIN.
- RP-428: EXPLAIN still reports an option's error ahead of a missing
  relation, a missing function or a bad literal.

### SQL server: arbitrary WHERE expressions and three-valued logic

The sqllogictest random corpus writes SQL the way a fuzzer does — `WHERE
- col2 + col1 IS NOT NULL`, `WHERE 1 IN (2)`, `SELECT + + 90 * a * - b` —
and the planner used to reject anything its Mongo-filter pushdown couldn't
express. Untranslatable WHERE clauses now route to per-row evaluation
automatically (a dry-run of the lowering decides), computed unary
projections type correctly instead of crashing tag inference, `ORDER BY
<ordinal>` resolves to the output expression on the evaluated path, and the
scalar evaluator's NOT/AND/OR/BETWEEN implement SQL's three-valued logic
(`NOT NULL` is NULL, `NULL AND FALSE` is FALSE — visible under NOT).

#### Fixed

- `planner.py`: `where_needs_per_row` dry-runs the pushdown lowering and
  falls back to per-row evaluation when it raises; the DISTINCT plan path
  consults it too; `_infer_scalar_tag` types `- col` from its operand;
  `ORDER BY 1` resolves the output ordinal (except SRF outputs, which sort
  post-expansion).
- `scalar.py`: three-valued NOT/AND/OR and a decomposed BETWEEN whose
  definitively-false arm dominates a NULL bound.
- A predicate the pushdown can't lower no longer errors 0A000; cross-type
  comparisons under per-row evaluation match nothing instead of raising
  Postgres' 42883 — a documented divergence (`tasks/backlog.md`).

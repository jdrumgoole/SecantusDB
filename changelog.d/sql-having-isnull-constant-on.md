### SQL: HAVING IS NULL forms, constant JOIN ON, duplicate join group keys

Round four of the sqllogictest corpus tail. `HAVING <operand> IS [NOT] NULL`
now lowers for bare-column, aggregate, and computed-over-group-key operands
(`HAVING (- col2) IS NOT NULL`) on both the single-table and join HAVING
lowerers. A constant JOIN ON condition (`LEFT JOIN tab0 ON 80 = 70`) folds
three-valued — TRUE joins every foreign row, FALSE/unknown joins none (INNER
drops the row, LEFT null-pads). And two join GROUP BY wrong-answer bugs: the
same bare column name grouped from two aliases (`GROUP BY cor1.col1,
cor0.col1`) collapsed to a single group key, and `SELECT DISTINCT` over
grouped join output never deduplicated.

#### Fixed

- `planner.py`: `_having_to_match` / `_join_having_to_match` lower
  `IS [NOT] NULL` over bare columns, aggregates, and computed group-key
  expressions (the last via `_to_agg_expr` over a group-key resolver, correct
  through any NOT nesting); `[NOT] <expr> IN (<exprs over group keys>)`
  lowers three-valued; always-unknown NULL-operand predicates
  (`HAVING NOT NULL IN (- col1)`, `NOT NULL NOT BETWEEN - col0 AND NULL`)
  fold to match-nothing; `_to_agg_expr` learns unary minus over non-literals.
- `planner.py`: an always-unknown JOIN ON (`ON NOT NULL < expr`) folds like a
  constant-false ON instead of raising.
- `planner.py`: `_lookup_stage` folds a constant ON via
  `_constant_predicate_filter` instead of raising "ON must compare columns".
- `planner.py`: duplicate bare column names in a join GROUP BY mint distinct
  grouped fields on both the join-group and join-group-window paths
  (qualified references rewrite/resolve onto the minted key); grouped
  `SELECT DISTINCT` over a join dedups with the same second `$group` the
  single-table planner uses.

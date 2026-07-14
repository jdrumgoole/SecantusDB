### SQL server: join-path aggregate expressions and WHERE residuals

The JOIN planners catch up with the single-table paths from the last two
rounds: aggregate arguments over joins can be expressions
(`MAX(cor0.col0 + 1)`, `SUM(- 83)` over a CROSS JOIN), lowered through the
join resolver with identity decorations stripped, and a join WHERE the
`$match` lowering can't express routes to the per-row residual the join
pipelines already carry (a dry-run probe, the join twin of the
single-table one) instead of erroring.

#### Fixed

- `planner.py`: computed-over-aggregate outputs over a join
  (`COUNT(*) * 32 FROM a CROSS JOIN b`) route to the group-then-evaluate
  builder instead of failing per-row; `_join_accumulator` lowers
  expression arguments for
  sum/avg/min/max; `_agg_key` identifies expression aggregates by SQL text
  instead of crashing the resolver; `_join_where_lowerable` dry-runs
  `_expr_to_filter` and the inner/outer join builders plus both join
  residual sites consult it.

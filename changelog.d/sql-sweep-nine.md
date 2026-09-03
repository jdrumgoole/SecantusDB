### A ninth SQL sweep: a named window was evaluated as `OVER ()`

Window functions were the richest surface probed here yet, and four of the
findings were silent — a plausible number in every row, computed from a window
that was not the one written.

**A named window lost its whole definition.** sqlglot keeps a `WINDOW w AS
(...)` definition on the SELECT and leaves the *reference* as a bare alias with
no partition, no order and no frame — which is exactly what every consumer
downstream reads. So `sum(v) OVER w` with `w AS (ORDER BY id)` returned the
whole-partition total on every row instead of a running one, and a
`PARTITION BY` in the definition was dropped just as quietly.

**`EXCLUDE` was parsed and then ignored.** It rides on the frame spec as
`args["exclude"]` and nothing read it, so `EXCLUDE CURRENT ROW` answered the
*unexcluded* frame — a running sum that still counted the current row.

**`NULLS FIRST` in a window `ORDER BY` was ignored.** NULLs were placed by
direction alone, which happens to reproduce PostgreSQL's defaults (last for
ASC, first for DESC) — so the flag looked right until somebody wrote it
explicitly, and then every rank in the partition was wrong.

**`sum` and `avg` were typed by rules the `GROUP BY` path had long since got
right.** A window `sum(int4)` declared int4 where PostgreSQL promotes to int8,
and `avg` declared float8 and divided as a float where PostgreSQL answers
numeric at `select_div_scale`'s scale.

#### Fixed

- **Named windows** (`WINDOW w AS (...)`) are resolved into their references
  before a planning path is chosen, so the rest of the engine only ever sees a
  fully specified window. Definitions may chain (`w2 AS (w1 ORDER BY x)`); a
  reference may add an `ORDER BY` and a frame but not override the definition's
  `ORDER BY` (`42P20`), and an unknown name is `42704`.
- **`EXCLUDE CURRENT ROW` / `GROUP` / `TIES` / `NO OTHERS`**, on aggregate and
  value windows alike. A frame is now an explicit index list, because `EXCLUDE`
  punches a hole in the middle that an `[lo, hi]` pair cannot express.
- **`NULLS FIRST` / `NULLS LAST`** in a window `ORDER BY`.
- **Window `sum` / `avg` result types**, via the same `_sum_tag` / `_avg_tag`
  helpers the `GROUP BY` path uses, and `avg` over an exact input now finishes
  in numeric arithmetic rather than float — `avg` over 10, 20, 20 is
  `16.6666666666666667`, not `16.666666666666668`.
- **`ntile`** types as int4; it is the one integer window function PostgreSQL
  does not make bigint.
- **`agg(...) FILTER (WHERE ...) OVER (...)`** no longer fails with `42803`
  naming an ordinary column. The `FILTER` node sits between the aggregate and
  its `Window`, and the "is this a window aggregate?" guard looked only one
  level up, so the aggregate was mistaken for a grouped one.
- **A subquery in `RETURNING`** — `INSERT ... RETURNING id, (SELECT count(*)
  FROM t)` — crashed with `XX000 internal error`: the `RETURNING` scalar context
  was built with `catalog=None`, so resolving the subquery's table raised
  `AttributeError`.

#### Added

- **`string_agg` and `array_agg` as window functions**, including under
  `FILTER`, with the array typed from its element rather than declared numeric.
- **`UPDATE ... SET (a, b) = (x, y)`**, in all three row-constructor spellings
  (`(a, b)`, a one-element `(a)`, and `ROW(...)`), expanded to single-column
  assignments at parse time. An arity mismatch is `42601`. A row *subquery*
  right-hand side is still unsupported and says so.

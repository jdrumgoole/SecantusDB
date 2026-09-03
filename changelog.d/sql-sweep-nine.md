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

- **`COALESCE`, `GREATEST` and `LEAST` resolve a common type** instead of taking
  the first argument's. `coalesce(1::int, 2.5::numeric)` declared int4 and then
  coerced the numeric result with `int('2.5')` — a bare Python `ValueError` that
  reached the client with *no SQLSTATE at all*. The old code carried a comment
  claiming first-wins "is what PG's common-type resolution amounts to for the
  shapes we can decide"; it is not, and a probe says so. Note the precedence is
  **not** arithmetic's: `int + real` is double precision, but
  `greatest(int, real)` is real.

- **`concat`, `concat_ws` and `format`'s `%s` render a boolean as `t` / `f`.**
  They go through the type's *output* function, not `::text` — which spells it
  `true` — so `concat(1, 2.5, true)` answered `12.5true` where PostgreSQL says
  `12.5t`. Bool is the only type where the two spellings differ.
- **`format` rejects too few arguments** with `22023` instead of substituting an
  empty string, positional (`%3$s`) forms included.
- **`split_part` with an empty delimiter** returns the whole string as field 1;
  Python's `str.split("")` raises, and the `ValueError` escaped as a confusing
  `function split_part(unknown) does not exist`. A zero field position is
  `22023`, as it is there.

- **A compound `INTERVAL` literal lost everything after its first two tokens.**
  sqlglot parses `INTERVAL '1 day 3:45:00'` as `Interval(this='1', unit=DAY)`
  and *discards the rest of the string* — it round-trips as `INTERVAL '1 DAY'`,
  so three hours and forty-five minutes were gone before any of this engine's
  code ran, and `INTERVAL '2 days ago'` came back **positive**. It only
  truncates when the text starts `<number> <unit>`; a bare `'3:45:00'` and a
  many-worded `'1 year 2 mons 3 days 04:05:06'` both survive, which is why it
  hid. Compound literals are now rewritten to a cast before parsing, beside the
  other repairs to sqlglot's parsing.
- **`INTERVAL '1-2'`** (the ISO year-month form) reached the wire as `XX000`;
  so did the full `'1-2 3 4:05:06'`, whose bare `3` is the days field rather
  than a value awaiting a unit.
- **Negating an interval COLUMN** raised a bare `decimal.ConversionSyntax` with
  no SQLSTATE: `- col` fell through to a `numeric` default for any non-numeric
  operand, and the output coercion then fed the interval subdocument to
  `Decimal`. The literal form was always fine.
- **`time ± interval` is a `time`**, wrapping at midnight, not an interval —
  `TIME '13:45' - INTERVAL '14 hours'` came back as a 23:45 *duration* under the
  interval oid.
- **A `::date` cast compares equal to `CURRENT_DATE`.** The cast yields the
  canonical text while `CURRENT_DATE` yields a `datetime.date`, so
  `now()::date = CURRENT_DATE` answered FALSE on a day when both plainly named
  the same one. Two casts, or two literals, were always fine.

#### Added

- **`string_agg` and `array_agg` as window functions**, including under
  `FILTER`, with the array typed from its element rather than declared numeric.
- **`LOCALTIME` and `LOCALTIMESTAMP`**, the tz-naive twins of `CURRENT_TIME` /
  `CURRENT_TIMESTAMP`; sqlglot gives them their own nodes and neither was
  handled, so both answered `42883 function localtime() does not exist`.
- **`min_scale` and `trim_scale`**, beside the `scale` that was already there —
  `scale` is the digits a numeric *carries*, `min_scale` the smallest that keeps
  the value exactly, and `trim_scale` the value re-scaled to it.
- **`UPDATE ... SET (a, b) = (x, y)`**, in all three row-constructor spellings
  (`(a, b)`, a one-element `(a)`, and `ROW(...)`), expanded to single-column
  assignments at parse time. An arity mismatch is `42601`. A row *subquery*
  right-hand side is still unsupported and says so.

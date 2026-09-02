### `ORDER BY` on a numeric column was an internal error

`SELECT n FROM t ORDER BY n` answered `XX000 internal error`. Sorting compares
stored values directly, and `Decimal128` — which is **every** `numeric` and
`money` value — implements no Python numeric protocol at all, so the comparison
raised a bare `TypeError`. An interval rides as a subdocument and had the same
problem.

It reached every sort path that does not delegate to Mongo: a plain
`ORDER BY`, a window's `OVER (ORDER BY …)`, `array_agg(x ORDER BY x)`,
`WITHIN GROUP (ORDER BY …)`, and every window aggregate except `count`, the one
that never looks at the value. `DISTINCT`, `GROUP BY` and `UNION` were
unaffected, because those sorts do go to Mongo — which is how a bug this plain
went unnoticed.

Decimal128 was wrong even where it did not raise: its equality compares the BID
encoding, so `1.0` and `1.00` were different values and `rank()` made two peers
into two ranks.

#### Fixed

- `ORDER BY` over `numeric`, `money` and `interval`, ascending and descending,
  with either NULL placement. Intervals order by duration, which is what
  PostgreSQL compares.
- `OVER (ORDER BY …)` for every window function, and `rank()` / `dense_rank()`
  now tie equal-but-differently-scaled numerics as the peers they are.
- Window `sum` / `avg` / `min` / `max` over `numeric`, `money` and `interval`.
  `min` and `max` follow PostgreSQL's fold, where an equal value replaces the
  running one — over 2.5, 1.0, 1.00 the minimum is `1.00`.
- `array_agg(x ORDER BY x)` and `percentile_cont` / `percentile_disc` / `mode`
  `WITHIN GROUP (ORDER BY x)` over the same types.

Ordering keys are now computed once per row rather than on every comparison,
which is where the normalisation lives.

#### Still unsupported

`ORDER BY` over `jsonb` or a range type. Both ride as subdocuments, and
PostgreSQL's total order over them is a slice of its own rather than a
coercion.

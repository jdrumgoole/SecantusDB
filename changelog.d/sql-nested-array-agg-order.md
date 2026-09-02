### A nested `array_agg` silently dropped its `ORDER BY`

`SELECT array_agg(i ORDER BY i DESC) FROM t` sorted. Wrap it in anything at all
— a cast, an operator, a subscript, another function — and it returned
**insertion order instead, with no error**.

`array_to_string(array_agg(x ORDER BY y), ',')` is the shape that makes this
look like ordinary SQL and get a wrong answer.

The top-level projection path pushed `{v, k}` pairs and sorted them in a
post-aggregate step. The registrar used for an aggregate nested inside a
computed projection registered a plain `$push` and never carried the ordering
at all — and `EvaluatedSelectPlan` had nowhere to carry it, so the fix adds the
same `post_aggregates` channel the pipeline plan already had.

#### Fixed

- An in-call `ORDER BY` is honoured wherever the `array_agg` appears: under a
  cast, an operator, a subscript, a function call, alongside other aggregates,
  grouped or not, and on the join planners as well as the single-table ones.
  Multi-key and per-key directions included.

#### Still refused

`GROUPING SETS` with any computed projection — `count(*)::text` is rejected
just as `array_agg(…)::text` is, so this is not about ordering. It stays an
honest `0A000`.

### The hypothetical-set aggregates

#### Added

- `rank`, `dense_rank`, `percent_rank` and `cume_dist` in their
  `f(value) WITHIN GROUP (ORDER BY expr)` form — what `value` would rank if it
  were inserted into the group. All four were `0A000`.

They share the ordered-set plumbing `percentile_cont` / `percentile_disc` /
`mode` already used, and needed a different payload and finish rather than new
machinery.

The sort direction is part of the answer and NULLs take part in the ordering,
so `rank(20) WITHIN GROUP (ORDER BY v)` and `... ORDER BY v DESC` give
different results on the same data — `ASC` defaults to `NULLS LAST` and `DESC`
to `NULLS FIRST`. `percent_rank` and `cume_dist` use different denominators
(`N` and `N + 1`), and only `cume_dist` counts the hypothetical row itself. On
an empty group the four answer 1, 1, 0.0 and 1.0 rather than NULL.

The multi-column form (one argument per `ORDER BY` expression) is refused with
`0A000` rather than answered approximately.

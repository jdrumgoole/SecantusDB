### `min()` and `max()` on a timestamp no longer answer a time that was never stored

`min(t)` over a column holding `12:00:00.123456` returned `12:00:00.123000`.
Not a rounding of the display — the query answered a timestamp that is not in
the table and never was. `max()` did the same, and `array_agg(x ORDER BY t)` and
`string_agg(x, ',' ORDER BY t)` ordered their output at millisecond granularity,
leaving rows that share a millisecond in storage order.

These are computed inside the aggregation pipeline rather than in the SQL layer,
which is why the read-side fix for `SELECT` and `ORDER BY` never reached them: a
BSON date holds whole milliseconds and the microseconds live in a separate
hidden field the accumulator never saw. They now accumulate the two together as
a single sortable value and recombine it on the way out. `FILTER (WHERE …)`,
all-NULL groups, and the join form are covered.

`HAVING` moved with them, and it is worth saying why. A clause like `HAVING
min(t) > '12:00:00.1230'` compares against the accumulator's output, so the
literal had to be handled in the same representation. Measured against
PostgreSQL across five operators before and after: three were already wrong,
one — `min(t) = '…122000'` — was right only because its literal happened to fall
on a whole millisecond, and all five are right now. Anything that changes this
area should re-measure `HAVING` rather than assume it follows.

Still not fixed, and recorded: `GROUP BY` on a timestamp merges rows that differ
only in microseconds, because the group key is still the truncated date.

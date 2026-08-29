### `ORDER BY` on a timestamp is microsecond-exact

Rows whose timestamps differed only below the millisecond came back in storage
order. Sorting four rows at `.122000`, `.123100`, `.123500` and `.123900` gave
the first row correctly and then the remaining three in the order they were
inserted, because the sort key was built from the stored millisecond value and
the microseconds live in a hidden companion field that the key never consulted.

This is easy to under-rate as a display nit, so it is worth being precise about
what it affected. `LIMIT` reads off the sorted list, so `ORDER BY t LIMIT 2`
returned the wrong *rows*. `DISTINCT ON` keeps the first row per group in the
`ORDER BY` order, so it silently picked a different row than PostgreSQL does.
Neither looks like a sorting bug from the outside — they look like wrong
answers.

It was two code paths, not one. Beyond the plain-column sort key, a query
ordering by an expression or a column ordinal — `SELECT id, t FROM x ORDER BY 2`
— runs through a separate route that evaluates against the source row, and that
route never restored the microseconds at all. So that shape both sorted at
millisecond granularity *and* returned truncated times. Both paths now read
through one helper.

Verified against a live PostgreSQL 14 across plain, `DESC`, aliased, ordinal,
`DISTINCT ON` and `LIMIT` shapes.

Not fixed, and now recorded with its measurement: `min()`, `max()` and the
in-call `array_agg(x ORDER BY t)` form are computed inside the aggregation
pipeline, which reads the stored date and never sees the companion. `min(t)` and
`max(t)` therefore still answer a whole-millisecond time for a stored
microsecond one.

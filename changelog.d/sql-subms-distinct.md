### `SELECT DISTINCT` on a timestamp keeps rows a microsecond apart

Two rows recorded at `.123100` and `.123500` came back as a single row reading
`.123000` — collapsed together, and reported as a time neither of them held.
`DISTINCT` deduplicates on the projected value, and the projection was dropping
the microseconds before the deduplication ever ran.

`count(DISTINCT t)` was wrong for the same underlying reason but by a different
route, and answered 2 where PostgreSQL answers 3.

Both are fixed, along with `DISTINCT *`, multi-column `DISTINCT`, and ordering
over deduplicated output.

This completes the sub-millisecond work: comparisons, reads, `ORDER BY`, `min` /
`max`, ordered aggregates, `GROUP BY` keys, `HAVING`, `DISTINCT` and
`count(DISTINCT …)` are now all microsecond-exact, verified across 39 query
shapes against a live PostgreSQL.

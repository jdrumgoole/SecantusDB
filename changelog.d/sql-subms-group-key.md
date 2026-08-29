### `GROUP BY` on a timestamp no longer merges rows that differ in microseconds

Grouping by a `timestamp` column used the whole-millisecond value as the key, so
rows recorded at `.123100` and `.123500` landed in the same group. Three
distinct times became two groups; `count(*)` answered 3 where PostgreSQL answers
2 and 1, `sum(id)` answered 6 where PostgreSQL answers 4 and 2, and the group
key came back as `.123000` — a time none of the rows held.

The aggregate values are the important part. A merged group does not just label
itself wrongly, it sums and counts rows that belong to different groups, and
nothing about the result looks suspicious.

`HAVING` over a grouping column moved with it, the same way it did for `min` and
`max`: the clause compares against the group key, so the literal is handled in
the same representation. All three `HAVING` shapes measured against PostgreSQL
were wrong before and are right now.

Still not fixed, and now measured precisely: `SELECT DISTINCT` on a timestamp
collapses rows the same way *and* returns a truncated value. It takes a third
route through the planner — a projection that drops the microseconds before the
deduplication runs — so neither this fix nor the earlier ones reach it.

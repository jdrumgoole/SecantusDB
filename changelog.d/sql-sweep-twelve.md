### A twelfth SQL sweep: `LIMIT 0` returned every row

Transactions came back **perfect — 40 of 40**: savepoints, the
aborted-transaction state, isolation levels, `READ ONLY`, and transactional DDL
rollback all match PostgreSQL 14.13. Two clusters did not.

**`LIMIT 0` returned the whole table.** A sentinel collision: the planner used
`0` to mean "no `LIMIT`", and every consumer tested the value for truthiness, so
a real `LIMIT 0` was indistinguishable from its own absence. It matters because
`LIMIT 0` is how a client asks for a result's column metadata *without* rows —
ORMs and BI tools do it constantly.

The fix runs deeper than the sentinel. The storage layer reads `limit=0` as "no
limit" too (Mongo's convention), so a genuine `LIMIT 0` must never reach it; and
Mongo's `$limit` stage *rejects* zero outright (`54000 the limit must be
positive`), so the pipeline emits a match-nothing stage instead.

**Rows containing NULL ignored SQL's three-valued rules.** `(1,NULL) =
(1,NULL)` answered true where PostgreSQL says NULL — Python's `==` treats two
`None`s as equal — `(NULL,NULL) IS NULL` answered false where it says true, and
`(1,2) < (1,NULL)` raised `42883` naming `integer[]`, the record having been
compared as an array.

#### Fixed

- **`LIMIT 0` returns no rows**, on every path: the plain scan, the aggregation
  pipeline, a derived table, and a FROM-less `SELECT`.
- **`LIMIT NULL` and `LIMIT ALL`** mean no limit, as they do in PostgreSQL; a
  negative limit is `2201W`; and `FETCH FIRST ROW ONLY` (the standard's optional
  count, defaulting to one) works instead of returning everything.
- **A FROM-less `SELECT` honours `LIMIT` / `OFFSET`** at all — `SELECT 1 OFFSET
  1` is empty.
- **Row comparison follows the three-valued rule**: fields are compared left to
  right, the first pair that decides wins, and a NULL pair reached before then
  makes the whole comparison NULL. `(1,NULL) = (2,3)` is still false, because
  the *first* pair decided it.
- **`row IS NULL` / `row IS NOT NULL`** are each true only when *every* field
  qualifies — so a row with one NULL is false for **both**. They are not
  negations of each other, and treating them as such made `(1,NULL) IS NOT
  NULL` true.

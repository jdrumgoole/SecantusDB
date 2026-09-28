### `GROUPING SETS`, `ROLLUP` and `CUBE` on the Rust PostgreSQL server

The clause used to be parsed and then discarded: the `GroupingSet` node fell
through to the expression arm, failed to resolve as a column, and the statement
died with `42803 column "a" must appear in the GROUP BY clause` — an error
blaming the user's own query for a clause the server had dropped. That is the
second of the two such cases the 2026-09-28 survey found, and the last one.

#### Added

- `GROUP BY GROUPING SETS (...)`, `ROLLUP (...)` and `CUBE (...)`, including
  several constructs in one `GROUP BY` (their sets are crossed). Each set groups
  on its own keys and NULL-pads the others; sets concatenate in declared order
  and duplicate sets emit duplicate rows, as PostgreSQL 14.13 does.
- The empty set `()` groups the whole input into one row, and still returns that
  row when the input is empty.

#### Known limitation

- The `GROUPING(col)` function is refused `0A000`, and the refusal names it
  rather than answering the generic "this target is not supported yet". It
  reports which set produced a row, which needs the producing set carried
  through the group.

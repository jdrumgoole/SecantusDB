### `cume_dist()` and `percent_rank()`

Both window functions were unavailable. Every other window function already
worked, so these two were the gap.

Both are peer-aware: rows that tie under the `ORDER BY` share a value, so
`cume_dist()` counts the whole tied group rather than the row's own position.

#### Added

- `cume_dist()` and `percent_rank()`, including partitions, tied rows, and a
  single-row partition.

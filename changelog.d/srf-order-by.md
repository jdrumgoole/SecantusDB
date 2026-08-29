### `ORDER BY` works over `unnest`

Sorting the rows produced by `unnest` didn't. `SELECT unnest(ARRAY[9,8,7]) AS u
FROM src ORDER BY 1` returned the elements in array order, and ordering by the
column's own name — `ORDER BY u`, the form most queries use — failed outright
with "feature not supported".

Both came from the same thing: the sort key was computed once per source row,
before the function expanded it into many. Every expanded row therefore carried
an identical key, and sorting them left the original order untouched. Keys for a
set-returning column are now taken from the expanded row.

`DISTINCT ON` is unaffected: its key is deliberately row-level and still
computed before expansion, which is what PostgreSQL does.

The equivalent field-selection form,
`(information_schema._pg_expandarray(arr)).x`, plans differently and is not
covered by this change.

#### Fixed

- `ORDER BY` over `unnest` sorts the expanded rows, by output ordinal or by
  column name, ascending or descending.

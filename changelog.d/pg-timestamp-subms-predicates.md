### Filters and grouping see a timestamp's microseconds

On the Python PostgreSQL server, some queries compared timestamps only to the
millisecond, and so returned the wrong rows when two values differed only in
their microseconds:

- A `WHERE` on a `timestamp[]` column (`a = ARRAY[...]`, `x = ANY(a)`,
  `a @> ...`) found nothing when the literal had microseconds, and `a <> ...`
  returned the equal row as well.
- A query with `EXISTS` or a correlated subquery that compared timestamps
  (`EXISTS (... WHERE s2.t = s.t)`) never matched.
- `DISTINCT` and `GROUP BY` on a `timestamp[]` column merged arrays that
  differed only in their microseconds, so counts came out too low.

All three now compare the full value. The data was always stored correctly;
these paths read it without the stored microseconds.

#### Fixed

- `sql/planner.py`: a `WHERE` that reads a timestamp-array column is evaluated
  row by row; `DISTINCT` / `GROUP BY` on one groups on the full value.
- `sql/executor.py`, `sql/scalar.py`: the row-by-row `WHERE` reads columns with
  their microseconds, for the outer query and for the rows of a subquery.

#### Testing

- `tests/test_pg_timestamp_subms_predicates.py`: the array predicates, a
  correlated `EXISTS`, and `DISTINCT` / `GROUP BY` counts and values. All six
  tests fail without the fix.

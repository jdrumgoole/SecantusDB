### HAVING compares a numeric sum at full precision

On the Python PostgreSQL server, `select g from t group by g having sum(v) =
<the value the select list returned>` could return no rows. The select list
sums a `numeric` column exactly. HAVING compared an in-pipeline running total
instead, which rounds at 34 significant digits and ignores a value too wide
for a Decimal128. A group whose sum held a 50-digit value compared as if that
value were not there.

For a single-table GROUP BY, including with window functions, and for an
aggregate with no GROUP BY, a `numeric` `sum` / `min` / `max` in HAVING is now
evaluated after the exact fold. Joins, grouping sets and `FILTER` terms still
compare in the pipeline (tracked in `tasks/backlog.md`).

#### Fixed

- `sql/planner.py`: `_having_to_match` sends a numeric sum / min / max term to
  the per-grouped-row HAVING residual when the caller has one.

#### Testing

- `tests/test_pg_having_exact_numeric.py`: 37-digit and 50-digit sums,
  `min` / `max`, `BETWEEN`, `IS NULL`, a mixed `AND`, ORDER BY + LIMIT, a
  window over the groups, and a whole-table aggregate.

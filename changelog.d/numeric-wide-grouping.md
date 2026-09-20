### Wide numerics group by value

On the Python PostgreSQL server, `select v, count(*) from t group by v` over a
`numeric` column returned one row per stored *text* rather than one per value.
A number past 34 significant digits does not fit a Decimal128, so it is stored
with its Postgres text beside a scale-free key — which made `1e40`, `1e40.0`
and `1e40.00` three documents and therefore three groups where PostgreSQL has
one. `SELECT DISTINCT` and `count(DISTINCT v)` split the same way.

Such a column now groups on the value, and the group carries the first row's
value for display, which is what PostgreSQL prints: insert `…890.00` first and
GROUP BY answers `…890.00`, insert the bare form first and it answers that
(measured against PostgreSQL 14.24).

Still split, and recorded in `tasks/backlog.md`: a join whose key is wide, and
a value stored narrow in one row and wide in another (`1.5` against `1.5`
written with 39 digits).

#### Fixed

- `sql/numeric.py`: `group_key_expr`, the value identity for a numeric column.
- `sql/planner.py`: every `$group` that keys on a numeric column — plain GROUP
  BY, GROUPING SETS / ROLLUP / CUBE, the window and join planners, `SELECT
  DISTINCT` and the DISTINCT-over-grouped-output dedup — keys on that identity
  and restores the display value.

#### Testing

- `tests/test_pg_numeric_wide_grouping.py`: pinned cases, plus ten that run the
  same statements against a live PostgreSQL and compare the text, so a
  difference in scale fails rather than passing silently.

### SQL: three-valued NULL semantics on the pushdown, and the aggregate long tail

The SQL server's Mongo-filter pushdown now honours SQL's three-valued logic: `<>`,
`NOT (...)`, `NOT BETWEEN`, and `NOT IN` no longer match rows whose operand column
is NULL (Mongo's `$ne`/`$nor`/`$nin` are two-valued and matched them), a NULL
candidate in an `IN` list can no longer match a NULL row, and `x NOT IN (…, NULL)`
correctly matches nothing. `SUM` over zero non-null inputs returns NULL instead of
Mongo's 0, on every plan path. Alongside, a round of sqllogictest-corpus aggregate
and planner shapes: FROM-less aggregates (`SELECT COUNT(*)` is 1), `COUNT(<expr>)`
counting non-null evaluations, expression `DISTINCT` aggregate arguments
(`SUM(DISTINCT 77)`), computed and constant projections under GROUP BY, `SELECT *`
grouped by every column, `SELECT DISTINCT` over grouped output, parenthesized join
sources (`FROM (a CROSS JOIN b)`), constant-LHS `IN` (list and subquery forms),
division by zero raising SQLSTATE 22012, and Postgres-exact `float8` wire text
(`12`, not `12.0`; `NaN`/`Infinity` spellings). Three files of the corpus's
`random/` suites now pass end-to-end that previously failed on their first record.

#### Fixed

- `planner.py`: `_negated_filter` lowers `NOT` by pushing the negation into the
  tree (De Morgan, comparison-operator flips, null-guarded single-field fallback)
  instead of Mongo's two-valued `$nor`; `<>` is null-guarded; `$in` lists drop
  NULL candidates; constant-LHS `IN`/`NOT IN` fold three-valued (list + subquery);
  a NULL comparison operand folds to match-nothing even when wrapped
  (`51 <> (NULL)`, `- CAST(NULL AS INT) <> x`); computed comparisons lowered to
  `$expr` guard both sides non-null (BSON total order is two-valued —
  `NULL <> 19` matched every row).
- `planner.py`: a join WHERE the `$match` lowering can't express routes to the
  per-row evaluated join / the pre-group residual instead of being silently
  dropped, on both the plain-join and the join-group-window paths
  (`WHERE (NULL) BETWEEN NULL AND NULL` returned every row).
- `planner.py`: two *different* expression aggregates of the same function
  (`MAX(3)` and `MAX(-94 - -16)`) no longer collide on the `(func, None)`
  accumulator-dedup key and share one value; integer `/` inside aggregate
  arguments and `$expr` lowers with PG's truncate-toward-zero semantics
  (`MIN(col1 / -99)` was computed with real division).
- `planner.py` / `executor.py`: `SUM` over only-NULL inputs is NULL on the plain
  group, group-window, join, join-window, and DISTINCT paths; the evaluated group
  path synthesizes the one implicit-aggregate row over empty input like the
  pipeline path already did.
- `planner.py`: FROM-less SELECTs fold aggregates over their one implicit row;
  `COUNT(<literal>)` no longer misroutes to the lone-`COUNT(*)` fast path;
  `COUNT(<expr>)` counts non-null evaluations (`COUNT(NULL)` is 0).
- `planner.py`: expression `DISTINCT` aggregate arguments push the lowered
  expression into the distinct set (single-table, group-window, join, join-window
  registrars); computed-over-aggregate outputs over a JOIN route to the
  group-then-evaluate builder (`COUNT(*) * COUNT(*)`).
- `planner.py`: grouped SELECTs with computed/constant projections route to the
  evaluated group path; `SELECT *` under GROUP BY expands when every column is a
  group key; `SELECT DISTINCT` over grouped output dedups; `ORDER BY <ordinal>`
  resolves on the group-then-evaluate path.
- `planner.py`: `FROM (a CROSS JOIN b)` unwraps grouping parens instead of
  erroring "a derived table requires an alias".
- `scalar.py`: division / modulo by zero raise SQLSTATE 22012 instead of leaking
  an internal error; `COALESCE` evaluates lazily like Postgres, so a
  division-by-zero in a never-reached argument no longer raises; operand-form
  `CASE x WHEN v` uses SQL equality (a NULL operand or WHEN value never
  matches, where Python `==` matched NULL to NULL).
- `planner.py` / `executor.py`: a constant `HAVING` (``HAVING NOT NULL IS
  NULL``) folds three-valued to match-all / match-nothing; DISTINCT aggregates
  over zero input rows synthesize their NULL row instead of crashing on the
  ``$addToSet`` reduction ("$size requires an array").
- `typemap.py`: `float8` text output uses Postgres' shortest form (`12`, `-0`,
  `1e+20`, `NaN`, `Infinity`).
- `planner.py`: `_infer_scalar_tag` is memoized per statement — deep arithmetic
  chains were exponential (a 20-term sqllogictest expression took ~0.5s; whole
  corpus files timed out).

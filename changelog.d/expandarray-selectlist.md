### Set-returning functions in the SELECT list, and search_path-aware visibility

`information_schema._pg_expandarray` now works in the SELECT list — bare
(a composite `(x, n)` column, one output row per array element) and with
immediate field access (`(SRF(arr)).n`), with multiple references to the
same call expanding in lockstep and empty arrays eliminating the row,
as PostgreSQL does. Composite field access `(col).x` also lowers inside
JOIN ON conditions. Together these are the exact call sites pgjdbc's
`DatabaseMetaData.getPrimaryKeys` / `getPrimaryUniqueKeys` queries emit,
which back JDBC updatable ResultSets.

`pg_table_is_visible()` now honours the session's `search_path` instead
of a hardcoded default-namespace list — `SET search_path TO schema1`
previously made every user-schema relation invisible to the predicate,
so a same-named table in two schemas could not be disambiguated (the
pgjdbc updatable-resultset probe hit exactly this). The function also
works as a projected value in any expression context, alongside
`current_database()` / `current_schema()`.

#### Added
- Record SRFs in the SELECT list (`_pg_expandarray`), FROM-ful and
  FROM-less, with lockstep multi-reference expansion.
- `(col).field` composite access in JOIN ON.
- `current_database()` / `current_catalog` / `current_schema` /
  `pg_table_is_visible()` in per-row expression contexts.

#### Fixed
- `pg_table_is_visible` WHERE lowering follows the session search_path
  (was a hardcoded public/pg_catalog list).

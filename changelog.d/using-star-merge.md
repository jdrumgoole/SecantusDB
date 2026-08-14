### SELECT * over a USING join merges the join column, like Postgres

`SELECT * FROM a JOIN b USING (k)` now returns `k` once — from the left
side (the right for RIGHT joins, `COALESCE` for FULL) — followed by each
source's remaining columns, exactly as PostgreSQL expands it. Previously
the star emitted the column once per side, a long-pinned divergence. The
fix is one AST rewrite before the USING-to-ON desugar, not a change to
every star-expansion path. `tbl.*` items over joins also work now (they
previously crashed with `column "*" does not exist`), and — matching
Postgres — `tbl.*` does NOT merge; only the bare `*` does.

#### Fixed
- Bare `*` over `USING` joins merges the join columns (left / right /
  coalesce per join side; chained USING joins in the all-inner case).
- `tbl.*` in a join select expands to the table's columns instead of
  crashing.

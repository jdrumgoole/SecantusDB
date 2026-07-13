### SQL server: bare COPY options and computed projections over row sources

Two more gauge-driven fixes. `COPY … TO STDOUT (FORMAT csv)` — the
options spelling psycopg emits, without `WITH` — now parses (sqlglot only
accepts the `WITH (…)` form, so `parse()` inserts it, anchored on the
STDIN/STDOUT target and a known option keyword). And projections that
compute over a set-returning or catalog row source — `SELECT x * 2 FROM
generate_series(1,3) AS t(x)`, `SELECT 1 FROM pg_namespace` — run through
the per-row evaluated plan instead of failing with "expected a column".
The psycopg-gauge subset stands at 685 of 979 (70%), from 42% at the
first run.

#### Fixed

- `planner.py`: `COPY … TO STDOUT/FROM STDIN (options)` normalizes to the
  `WITH (options)` spelling sqlglot parses; both the table and the query
  form take options.
- `engine.py`: SRF and virtual-catalog row sources route computed
  projections (arithmetic, literals, scalar functions) through the
  evaluated-select plan — execution and Describe agree on the shape.

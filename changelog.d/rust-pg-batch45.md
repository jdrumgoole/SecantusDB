### Rust PostgreSQL server: operator type checks, built-in EXECUTE, view definitions

#### Fixed

- Four comparisons across types used to give a silent wrong answer. Each now raises PostgreSQL's error (42883 / 22007):
  - `arr = 1` returned the row;
  - `a = ANY(ARRAY['x'])` returned no rows;
  - `text BETWEEN 1 AND 2` returned no rows;
  - `current_date = 'x'` returned no rows.
- Other type and placement errors:
  - `sum(text)` returned 0;
  - an aggregate or window function in WHERE was accepted;
  - json `->` / `->>` operand types were not checked;
  - `int + float8` typing now matches PostgreSQL.
- After `REVOKE EXECUTE` on a built-in function, a direct call is refused. The check uses the overload chosen for the argument types.
- `pg_get_viewdef` prints cross-type date/time comparisons, `[NOT] MATERIALIZED` CTEs and RANGE offsets. Invalid RANGE offsets are refused at planning.
- Error positions and hints for the ungrouped-column and invalid-reference errors.

#### Changed

- A test now proves the catalog-cache gate is needed: without it, one connection's uncommitted event trigger fired on another connection's DDL.

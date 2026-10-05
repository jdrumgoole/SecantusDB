### Rust PostgreSQL server: savepoint-safe UNIQUE indexes, READ COMMITTED reads after a write, EXCEPTION blocks

#### Fixed

- A UNIQUE index created in a transaction became a plain index if the transaction was later moved: by `ROLLBACK TO SAVEPOINT`, a conflict re-run, or a snapshot refresh. Duplicates were then accepted after COMMIT, silently. The index's options are now kept.
- A READ COMMITTED block that has written now sees other sessions' later commits in its following statements, as PostgreSQL does. This applies while its write set is at most 256 entries.
- A PL/pgSQL `EXCEPTION` block releases the rows it undoes immediately.
- Every transaction move keeps the block's rows held across the move, closing a window where another writer could take them.
- `FOR UPDATE` through a JOIN on a table without a primary key locks only the rows returned.
- `sum(real)` adds in single precision. It raises 22003 on overflow instead of returning Infinity.
- `round` / `trunc(numeric, n)` honour a negative `n` and handle NaN and Infinity.
- A correlated subquery no longer raises a data error from a select-list expression on a row no outer row reaches.

#### Performance

- More correlated-subquery shapes run once per statement rather than once per outer row:
  - numeric ordering filters;
  - `sum` / `avg` of real;
  - immutable built-ins in the select list;
  - `generate_series` / `unnest` of constants.

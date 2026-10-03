### Rust PostgreSQL server: SELECT FOR UPDATE, row-lock residuals, type checks, faster correlated subqueries

#### Fixed

- `SELECT ... FOR UPDATE` / `FOR NO KEY UPDATE` was accepted and ignored. Two sessions could both read and then update a row: a lost update. Row locks are now taken, including `NOWAIT` (55P03) and `SKIP LOCKED`.
- Row waits and deadlocks:
  - Autocommit statements, a running statement and primary-key INSERT conflicts now hold their rows while waiting, so cycles through them are 40P01.
  - `ROLLBACK TO SAVEPOINT` releases the rows written after the savepoint.
  - REPEATABLE READ waits for the holder before deciding on 40001.
  - A block re-run after a conflict no longer writes its failed attempt twice.
- Type mismatches that returned a value now raise PostgreSQL's error:
  - `CASE` / `COALESCE` / `GREATEST` / `LEAST` / `NULLIF` over mismatched types;
  - `x IN (SELECT ...)` across types.
- `greatest(date, timestamptz)` returns a timestamptz.
- `pg_get_viewdef` matches PostgreSQL on 60 more view shapes.
- `txid_current()` and `pg_current_xact_id()` work. `pg_current_snapshot()` reports running transactions.
- DDL notices ("skipping", the CASCADE list) are sent as they are raised.

#### Performance

- Correlated subqueries with a text key, a JOIN or an aggregate under a filter are hash-indexed: 2,000 × 2,000 rows went from about 1.2 s to 0.01-0.1 s.
- UPDATE / DELETE no longer decode the whole catalog to find foreign keys. Planner setup is cached per session.

| µs per statement (release) | before | after | PostgreSQL 15 |
| --- | --- | --- | --- |
| extended `select 1` | 49 | 42 | 27 |
| extended autocommit UPDATE | 86 | 74 | 56 |

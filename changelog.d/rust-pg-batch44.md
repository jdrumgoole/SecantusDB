### Rust PostgreSQL server: READ COMMITTED waits for a row instead of failing

#### Fixed

- A READ COMMITTED block that met another session's uncommitted row used to fail at once with 40001. It now waits for that session, then re-runs its statement against the committed row, as PostgreSQL does. The final result matches PostgreSQL in every scenario tested. The wait still ends on:
  - cancel;
  - `statement_timeout`;
  - `lock_timeout` (55P03);
  - a deadlock (40P01).
- REPEATABLE READ and SERIALIZABLE still raise 40001.
- An UPDATE that leaves a row unchanged (`n = n*2` over 0) now takes the row lock. Before, it ignored another session's uncommitted update: it left 7 where PostgreSQL gives 14.
- A failed block rolls back its storage transaction immediately, so a deadlock's survivor need not wait for the loser's ROLLBACK.
- Privilege checks:
  - a SERIAL column's default `nextval` is checked for the inserting role;
  - `lastval()` is checked against its sequence;
  - EXECUTE on the trigger function is checked at `CREATE TRIGGER`;
  - grants on built-in functions are per signature.

#### Performance

A statement's Describe result is cached per session while the catalog, settings and role are unchanged. Release build, before and after:

| statement | before | after |
| --- | --- | --- |
| primary-key read | 104 µs | 77 µs |
| autocommit update | 101 µs | 84 µs |

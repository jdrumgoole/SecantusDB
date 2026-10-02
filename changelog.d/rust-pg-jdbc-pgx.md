### Rust PostgreSQL server: pgx is clean, pgjdbc runs to completion

The pgx gauge against the Rust PostgreSQL server went from 357 passed / 20 failed to 377 / 0. Before this change the pgjdbc gauge hung forever. It now completes: 5,475 of 5,637 tests pass and 134 fail. Each fix was checked against PostgreSQL 15 and pinned in the new `jdbc_pgx` corpus.

#### Fixed

- Several silent wrong answers:
  - A row error raised while rows streamed (`select 0/0 from t`) left the batch's earlier writes committed and the block not aborted.
  - `DEFERRABLE UNIQUE` was never enforced.
  - A data-modifying CTE ran again at Parse and Describe.
  - `5/count(*)` over no rows answered NULL instead of `22012`.
- Two hangs:
  - `nextval` deadlocked on its own transaction after a table was dropped and re-created in one block.
  - An extended-protocol group kept its table and advisory locks after `Sync`.
- Temporary tables are per session (`pg_temp_N`): they shadow permanent tables and are dropped at disconnect and at `DISCARD TEMP`.
- Prepared statements and portals:
  - `DEALLOCATE` and `DISCARD ALL` also drop wire-level statements.
  - A failed Bind aborts the block.
  - A changed result shape reports `cached plan must not change result type`.
  - A suspended portal resumes.
  - `ROLLBACK TO SAVEPOINT` reports the right transaction status.
- Startup:
  - Startup parameters and `options -c` are applied.
  - `server_version` is reported as 15.0.
  - `application_name` is reported.
  - Protocol 3.2 requests are negotiated down to 3.0.

#### Added

- `DISCARD`.
- `current_schemas(bool)`.
- `regproc` over the built-in functions.
- Empty-query responses.
- `22021` for a NUL byte in a text parameter.
- Binary `COPY` ending without its trailer.
- The pgjdbc runner creates its `test` database.

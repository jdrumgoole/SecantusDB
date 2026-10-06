### Rust PostgreSQL server: indexed and in-block reads stream, and four wrong answers fixed

The Rust PostgreSQL server streams more reads in bounded memory. A SELECT with
an indexed WHERE now walks its index a batch at a time instead of building the
whole result first, and so does an aggregate over an indexed filter. A
simple-protocol SELECT inside a REPEATABLE READ or SERIALIZABLE block streams
through the block's own transaction. A DISTINCT over a `tsvector` or `tsquery` column streams too.
Over 300,000 rows of 2 KB, server memory growth fell from 1.3 GB to 27 MB for
an indexed range, and from 2.6 GB to 47 MB for a whole-table read inside a
REPEATABLE READ block. The answers are byte-identical.

Reading one row by primary key no longer asks the storage for a query plan
first. That makes a PK read about 3 microseconds faster.

Probing these paths against PostgreSQL 15.19 found four wrong answers, all now
fixed. `exists(select 1/0)` raised an error, but PostgreSQL never evaluates an
EXISTS subquery's select list and answers true. A `SELECT DISTINCT` whose
ORDER BY was not in the select list returned rows where PostgreSQL raises
42P10. Float `power` returned NaN or Infinity where PostgreSQL raises an
error. Float `sqrt`, `ln` and `log` raised errors with the wrong SQLSTATE.

#### Added

- `secantus-storage`: `Storage::scan_routed_batches`, a batched scan routed
  through the same index `find_matching` would use.
- `tools/probes/plpgsql_expr_context.py`: compares the CONTEXT of PL/pgSQL
  expression errors against PostgreSQL over 60 expression shapes.

#### Changed

- `secantus-pgserver`: a simple-protocol or whole-result extended SELECT with
  an indexed WHERE streams; a primary-key point lookup keeps the direct path.
- `secantus-pgserver`: an aggregate over an indexed filter is computed in
  bounded memory, like one over a collection scan.
- `secantus-pgserver`: a simple-protocol SELECT inside a REPEATABLE READ /
  SERIALIZABLE block streams through the block's transaction.
- `secantus-pgserver`: a DISTINCT over a tsvector / tsquery column streams,
  de-duplicating on the value's text.

#### Fixed

- `secantus-pgplan`: an EXISTS subquery's select list, DISTINCT and ORDER BY
  are dropped before it runs, so `exists(select 1/0)` is true
  (PostgreSQL's `simplify_EXISTS_query`).
- `secantus-pgplan`: `SELECT DISTINCT ... ORDER BY <something not in the
  select list>` is 42P10 `for SELECT DISTINCT, ORDER BY expressions must
  appear in select list`.
- `secantus-pgplan`: the float math functions follow PostgreSQL's `float.c`:
  - `sqrt` of a negative raises 2201F.
  - `ln` / `log` of zero or a negative raises 2201E, with PostgreSQL's two
    messages.
  - `exp` overflow and underflow raise 22003.
  - `power` raises 2201F for a negative to a fraction or zero to a negative,
    and 22003 on overflow or underflow. Its NaN and infinity cases match.
- `secantus-pgserver`: a PL/pgSQL expression error carries the
  `SQL expression "..."` CONTEXT frame exactly when PostgreSQL's planner would
  raise it. That happens when a constant subexpression fails as it is folded:
  `1/0 + x` gets the frame and `x/0` does not. An unreadable literal never gets
  the frame. 58 of 60 probed shapes now match, up from 50.
- tests: the nested pytest runs no longer sweep the stale temp backlog at
  session start. That sweep was the cause of a 300 s timeout in
  `test_tmp_retention_guard.py`.

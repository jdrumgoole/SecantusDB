### Extended query protocol: three error-surface fixes

The Parse/Bind/Describe/Execute path is what psycopg, JDBC and most ORMs
actually speak, and it is a different server path from the interpolated SQL a
literal test corpus produces. Its value surface came back clean — every scalar
type through Bind, parameters in predicates, select lists, `LIMIT` and
`RETURNING`, and server-side cursors — but three statements were accepted that
PostgreSQL refuses.

#### Fixed

- `EXPLAIN` with a parameter no longer violates the wire protocol. It described
  itself as returning no rows and then sent rows anyway, which clients report as
  `server sent data ("D" message) without prior row description`. Only a
  parameterised `EXPLAIN` reaches this path, because a parameter is what makes a
  driver switch away from the simple protocol.
- `DECLARE CURSOR` outside a transaction block now raises `25P01`, as PostgreSQL
  does, instead of being accepted and then failing the following `FETCH` with
  `34000` — which reported the problem one statement late and blamed the wrong
  statement. `DECLARE ... WITH HOLD` is still allowed, since a holdable cursor
  survives the commit. The check applies to wire sessions only: the embedded
  `run_sql` API has no implicit commit, so a cursor declared there without a
  transaction stays usable, and the rule's rationale does not reach it.
- Parameters in DDL now raise `42P02 there is no parameter $1`. PostgreSQL binds
  parameters only into statements whose body is planned, so `CREATE TABLE t AS
  SELECT $1` is accepted while `CREATE VIEW v AS SELECT $1`, `CREATE INDEX`,
  `ALTER TABLE` and a `DEFAULT` or `CHECK` holding a placeholder are not.

#### Changed

- `tests/test_tmp_retention_guard.py` gives its nested `pytest` subprocesses
  their own `--basetemp` and a timeout. Without a basetemp a nested pytest
  registers an exit-time cleanup of every stale `pytest-of-<user>` directory,
  which can run for minutes after its tests have all passed; under `-n auto`
  the outer worker is killed waiting and xdist reports only `node down: Not
  properly terminated`, naming no test. Same diagnosis and same fix as the
  nested runs in `tests/test_crash_stall_watchdog.py`.

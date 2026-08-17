### Comma-joins are keyed, not cartesian — getImportedKeys no longer OOMs

A multi-table comma-join (`FROM a, b, c WHERE a.id = b.aid AND …`)
compiled each table to an UNKEYED `$lookup` that returned the whole
foreign collection per outer row — a cartesian product filtered only by
the terminal `$match`. Over several catalog tables that intermediate is
astronomical: pgjdbc's getImportedKeys for multi-column foreign keys (a
9-way comma-join over the catalogs) ballooned to **183GB and OS-killed
the server**.

The planner now pushes the WHERE equalities of the form `joined.col =
available.col` onto each comma-join's ON before building stages, so the
`$lookup` is keyed (comma-joins are INNER, so this is result-preserving;
residual predicates like single-table filters and array-subscript joins
stay in WHERE). The getImportedKeys query now completes in
milliseconds. A hard per-stage materialization cap
(`MAX_PIPELINE_DOCS`, 5M, env-overridable) is the backstop: any query
that still degenerates into an unbounded product fails with a clean
SQLSTATE 54000 instead of exhausting memory.

#### Fixed
- Comma-join `$lookup`s are keyed from WHERE equalities (getImportedKeys
  and similar catalog joins complete instead of cross-producting).
- Pipeline stages cap materialization at `MAX_PIPELINE_DOCS`, surfacing
  54000 (program_limit_exceeded) rather than OOMing the server.

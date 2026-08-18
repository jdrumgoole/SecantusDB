### Stored procedures: CREATE PROCEDURE / CALL / DROP PROCEDURE

SecantusDB now supports PL/pgSQL stored procedures. `CREATE [OR REPLACE]
PROCEDURE name(params) LANGUAGE plpgsql AS $$ … $$` parses (including the
`a INOUT int` argmode that sqlglot rejects — procedures are parsed by a
dedicated handler), stores the body with its per-parameter modes, and `CALL
name(args)` runs it over both the simple and extended wire protocols. A
procedure's `OUT` / `INOUT` parameters form its result row, so
`CALL proc(1)` on `proc(a INOUT int)` returns `1`; a procedure with no
output parameters returns no row. `RAISE NOTICE` inside a procedure body
surfaces as wire NoticeResponse messages, and `COMMIT` / `ROLLBACK` inside a
procedure are accepted (execution continues). `DROP PROCEDURE [IF EXISTS]`
removes a stored procedure.

#### Added

- `engine.py` / `planner.py` / `plpgsql.py`: `CREATE PROCEDURE` (parsed by a
  regex handler that accepts IN/OUT/INOUT/VARIADIC argmodes in either order),
  `CALL` returning OUT/INOUT parameters as the result row, `DROP PROCEDURE`,
  and PL/pgSQL `COMMIT` / `ROLLBACK` statements. `parameter_count` scans a raw
  Command's tail so `CALL proc($1)` binds its parameter in the extended
  protocol.

#### Known limitation

- `COMMIT` / `ROLLBACK` inside a procedure are accepted but do not create a
  mid-body transaction boundary in the CALL's autocommit context — a procedure
  that relies on committing part of its work and rolling back the rest is not
  modelled.

### Parse reports errors when it should, and ABORT works

The PostgreSQL server accepted a `Parse` it should have rejected. A statement
naming a table that does not exist, or input that is not SQL at all, got a
`ParseComplete`, and the error only came back later, at `Execute`. PostgreSQL
checks both when it parses, so a client that prepares a statement now and runs
it later (pgx's `Prepare`, JDBC with server-side prepare) saw the error on the
wrong message. Both are now reported in reply to `Parse`, and a syntax error
names the token PostgreSQL names: `this is not sql` is an error "at or near
`this`".

`ABORT`, PostgreSQL's synonym for `ROLLBACK`, was rejected as a syntax error.
That was more than cosmetic: inside a transaction that had already failed, the
`ABORT` meant to end it was itself refused, and the session stayed stuck until
the client sent `ROLLBACK`. `CHECKPOINT` was rejected the same way and is now
accepted.

`lower()` and `upper()` of a range now have the type PostgreSQL gives them:
`timestamp` for a `tsrange` (not `timestamp with time zone`), `date` for a
`daterange` (not a midnight timestamp), and a `tstzrange` bound keeps its
`+00` when cast to text.

`scripts/detached_run.py`, the helper for long runs, now works on Windows.

#### Fixed

- `sql/pgextended.py`, `sql/planner.py`: `Parse` resolves every relation with
  planning's own resolver (`planner.missing_relation`, which also accepts views
  and sequences) and answers `42P01` for a missing one.
- `sql/engine.py`: any bare expression that parses in place of a statement is a
  `42601` (`this is not sql` parsed as `NOT (this IS sql)` and slipped past a
  list of node types), and the error names the leading input token.
- `sql/planner.py`: `ABORT [WORK | TRANSACTION]` parses as `ROLLBACK`.
- `sql/engine.py`: `CHECKPOINT` answers its command tag (SecantusDB has no
  user-driven checkpoint to force).
- `sql/ranges.py`, `sql/planner.py`, `sql/scalar.py`, `sql/typemap.py`: the
  result type of a range bound follows the range type, and `::text` of a
  `lower()` / `upper()` looks through to the range column's type.
- `scripts/detached_run.py`: a Windows liveness check (`os.kill(pid, 0)` is not
  one there), `stop` without process groups, and a bare `python` resolved
  through `PATH`, so it is the venv's interpreter rather than the base one.

#### Testing

- `tests/test_pg_parse_analysis.py`: missing relations fail the `Parse`; 22
  valid relation kinds (views, sequences, temp tables, CTEs, catalogs,
  `UPDATE … FROM`, …) still parse; `ABORT` ends a failed block.
- `tests/test_pg_range_bound_types.py`: types and text for every range type,
  plus a real-PostgreSQL comparison in the `pg-oracle` lane.
- `tests/test_detached_run.py`: three tests that run on every platform (the rest
  are POSIX-only by nature, which is how the Windows breakage went unnoticed).

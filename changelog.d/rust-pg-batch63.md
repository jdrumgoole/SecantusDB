### Rust PostgreSQL server: read-only STABLE functions, and two silent wrong answers fixed

The Rust PostgreSQL server now runs a STABLE or IMMUTABLE function the way
PostgreSQL does: read-only. An INSERT, UPDATE, DELETE, DDL, SET, SHOW, NOTIFY
or row-locking SELECT inside one is refused with PostgreSQL's 0A000 "... is not
allowed in a non-volatile function" -- for a SQL function before any of its
statements runs, for PL/pgSQL at the offending statement. Before, the write
simply happened.

That guarantee is what lets a READ COMMITTED block call such a function
without replaying its writes: a read that calls only STABLE / IMMUTABLE
functions whose bodies read plain tables now reads in a fresh snapshot with
the block's own rows laid over it. Doing that exposed a silent wrong answer
that was already there -- a function over constants is run while the
statement is planned, which was before the block took its fresh snapshot, so
`select f()` missed a row another session had just committed. Such a read now
gets its snapshot before planning.

A subquery inside `INSERT ... VALUES` works (it was "SubLink is not supported
yet"), and a simple-protocol SELECT whose WHERE would be answered by a
collection scan anyway now streams its rows instead of building the whole
result first.

#### Fixed

- `secantus-pgserver`: a write (or any statement but a plain SELECT) inside a
  STABLE / IMMUTABLE function is 0A000, with PostgreSQL's CONTEXT
  (`SQL function "f" during startup`, or the PL/pgSQL statement frame). A
  VOLATILE function such a function calls may still write, as in PostgreSQL.
- `secantus-pgserver`: in a READ COMMITTED block that had written, a user
  function folded while planning read the block's old snapshot and missed
  rows other sessions had committed since (`select f()` answered 2 where
  PostgreSQL 15 answers 3).
- `secantus-pgplan`: a subquery in an `INSERT ... VALUES` row.

#### Changed

- `secantus-pgserver`: a READ COMMITTED read calling STABLE / IMMUTABLE user
  functions (SQL, or PL/pgSQL without dynamic SQL, cursors, CALL or EXCEPTION
  blocks, transitively, with no VOLATILE function anywhere) reads apart from
  the block under its overlay instead of replaying the block's write set.
- `secantus-pgserver`: a simple-protocol SELECT whose WHERE the storage would
  answer by a collection scan streams from a reader thread (300,000 rows of
  2 KB: server RSS growth ~700-1000 MB -> ~20 MB).

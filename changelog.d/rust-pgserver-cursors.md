### Cursors, and reading one in a loop

`DECLARE`, `FETCH`, `MOVE` and `CLOSE` now work on the Rust PostgreSQL server.
A cursor is declared inside a transaction — outside one it would be closed again
the moment the statement ended, and PostgreSQL refuses it for that reason — and
its result is read at declaration time, which is what makes it scrollable in
both directions afterwards.

PostgreSQL's idea of where a cursor *is* has two positions that are easy to
overlook, and both change the answers. The cursor sits on a numbered row, but it
can also sit before the first one or after the last. Fetching past the end parks
it after the last row rather than on it, so backing up two from there lands on
the last row and not the one before it. Getting that wrong is off by exactly
one, in the case people are most likely to try.

Three more rules that only a real server tells you: a backward fetch returns its
rows nearest-first rather than in table order; `RELATIVE` and `ABSOLUTE` fetch a
single row — the n-th from here, or the n-th from the start — where `FORWARD`
and `BACKWARD` fetch a run of them; and `FETCH ALL` arrives as a count so large
that any arithmetic on it has to be written not to overflow.

The bug worth naming is not in cursors at all. Clients prepare a statement they
run repeatedly — psycopg after five times — and a prepared statement is
described once and then executed. The describe path had no answer for `FETCH`,
so it reported that the statement returned no columns; the sixth read in a loop
then sent rows the client had no description for, which is a protocol violation
rather than a wrong answer. Reading a cursor in a loop is the ordinary way to
use one, so this was the normal case rather than an edge of it.

#### Added

- `DECLARE ... CURSOR FOR`, `FETCH`, `MOVE` and `CLOSE`, with PostgreSQL's
  position model, reverse-order backward fetches, and the `FORWARD` /
  `BACKWARD` / `ABSOLUTE` / `RELATIVE` directions.

#### Fixed

- A prepared `FETCH` described no columns, so reading a cursor in a loop broke
  once the client prepared the statement.

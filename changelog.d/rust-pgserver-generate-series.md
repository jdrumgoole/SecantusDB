### generate_series, and the rows a cursor needs to scroll over

`generate_series(start, stop)` — with an optional step, which may count
downwards — now works as a source in `FROM`. It is a *source* rather than a
statement of its own, and that is the whole design: `ORDER BY`, `LIMIT`,
`OFFSET` and the aggregates all operate on the generated rows without knowing
where they came from, so nothing had to be reimplemented for them.

Two rules are worth stating because they read the other way round. Counting up
towards a smaller stop produces *nothing* — `generate_series(5, 1)` is empty
rather than reversed, and counting down needs a negative step. And a step of
zero is refused rather than looped over, with the code PostgreSQL uses for an
argument whose value cannot work, which it keeps distinct from its general
data-error class.

The reason this was worth doing now is not the function itself. Cursors landed
in the previous release, complete and matching PostgreSQL across every operation
probed — and almost every test that used one still failed, because the usual way
to give a cursor rows to scroll over, without inventing a table first, is to
select from a generated series. A feature can be finished and still be
unreachable.

A `WHERE` clause over a generated source is refused rather than ignored. The
filter machinery here is built against stored columns, and quietly dropping a
predicate would return rows the client asked to exclude.

#### Added

- `generate_series` as a `FROM` source, with an optional and possibly negative
  step, column aliases (`AS g`, `AS g(x)`), `ORDER BY`, `LIMIT`, `OFFSET`, and
  `count` / `sum` / `min` / `max` over it.

### Rust PostgreSQL server: READ COMMITTED joins and aggregates without a replay, and pulled-up constants

A READ COMMITTED transaction block that has written, then reads other tables by
a join or an aggregate while other sessions commit, no longer replays its whole
write set before each read. Errors from inlined SQL functions over a one-row
constant FROM item now carry PostgreSQL's `during inlining` context.

#### Fixed

- `secantusd-pg`: a READ COMMITTED block's read by a JOIN, an aggregate, GROUP BY
  or an ORDER BY over an expression now runs in a fresh read-only transaction
  when every table it names is an ordinary table the block has not written. Before,
  the block was moved to a new snapshot with its writes replayed for every such
  statement. The read also has to call no function except pure built-ins, and
  have no subquery, no CTE and no row lock. 800 insert-then-join pairs beside a
  committing writer (debug build): 66.5 s -> 11.8 s.
- `secantusd-pg`: a one-row `VALUES` or constant `SELECT` in FROM is pulled up
  into a user function call's arguments, as PostgreSQL's planner does. An error
  raised by an inlined `LANGUAGE sql` function over that column now says
  `SQL function "f" during inlining`. Before, the error had no CONTEXT.

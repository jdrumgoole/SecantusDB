### Savepoints, and the nested blocks built on them

The Rust PostgreSQL server refused `SAVEPOINT` outright, and that refusal was
quietly expensive: every client builds a *nested* transaction block out of
savepoints, so `with conn.transaction():` inside another one failed even though
nothing in the user's code mentions the word.

They work now — `SAVEPOINT`, `RELEASE`, and `ROLLBACK TO`, with PostgreSQL's
rules: a repeated name shadows rather than replaces, rolling back to an outer
savepoint discards the ones nested inside it, releasing one keeps its writes
while leaving the enclosing savepoint still able to undo them, and rolling back
to a savepoint recovers a block that an error had aborted.

WiredTiger has no savepoint of its own, so one here is a set of pre-images:
before a statement writes a table, every open savepoint that has not yet
captured that table captures it, and rolling back puts the captured contents
back. Capturing lazily is what keeps it affordable — a savepoint nobody writes
through costs nothing at all.

Two more things fell out of the work. `CREATE TABLE IF NOT EXISTS` on an
existing table raised `42P07` instead of doing nothing, so the ordinary "create
it if it is missing" fixture failed the second time a session ran it. And the
aborted-block check now runs *before* the planner's answer, as PostgreSQL's
does: in an aborted block `select nosuchcolumn` is `25P02`, not `42703` — though
a syntax error is still reported as itself, because the parser runs first there
too.

#### Added

- `SAVEPOINT`, `RELEASE [SAVEPOINT]` and `ROLLBACK TO [SAVEPOINT]`, and so
  nested client transaction blocks.

#### Fixed

- `CREATE TABLE IF NOT EXISTS` raised `42P07` on an existing table.
- A statement in an aborted block reported its own error rather than `25P02`.

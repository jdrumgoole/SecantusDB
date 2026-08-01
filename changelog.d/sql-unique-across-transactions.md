### UNIQUE constraints hold against rows committed after your snapshot

A `UNIQUE` constraint could be violated from inside a transaction. Enforcement
worked by looking for an existing row through the transaction's own snapshot,
which is fixed when the transaction begins — so a row another connection
committed after that point was invisible, the check passed, and the duplicate
was stored. PostgreSQL rejects the same sequence, because a unique index is
checked against committed data even though your reads stay on your snapshot.

Enforcement now consults committed state as well as the transaction's own view.
Both are needed: the transaction's view sees rows it has inserted itself and
respects rows it has deleted, and the committed view sees what everyone else
has landed in the meantime.

Autocommit statements were never affected — each is its own short transaction —
which is why this went unnoticed. Nothing changes for them, and the extra check
costs nothing outside a transaction.

Two narrower cases still get through and are recorded in the backlog: a
transaction that has already written to the table before inserting, and two
transactions inserting the same value simultaneously. Both are closed properly
by making unique index entries collide in the storage engine, which is a
change to the on-disk layout.

#### Added

- `Storage.find_matching_committed`, a committed-state read for constraint
  enforcement (not for user-visible reads, which must keep their snapshot).

#### Fixed

- A `UNIQUE` constraint no longer accepts a value another transaction committed
  after the inserting transaction's snapshot was taken.

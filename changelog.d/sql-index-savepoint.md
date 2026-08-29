### `ROLLBACK TO SAVEPOINT` now undoes index changes

An index created after a savepoint survived a rollback to it, and an index
dropped after one stayed dropped. Everything else already behaved: tables,
columns and views created or dropped after a savepoint were all reverted
correctly, as was a full `ROLLBACK` of any index change. Indexes were the gap,
because savepoints work by snapshotting table contents and an index is not
stored as table contents.

Restored indexes keep what they were declared with — a unique index comes back
still rejecting duplicates, and a partial index keeps its filter — rather than
returning as a plain index of the same name.

The SQL guide previously stated the opposite, that DDL in general is not undone
by `ROLLBACK TO SAVEPOINT` and gave `CREATE TABLE` as the example. That was
wrong for every case it named and right only for the one it didn't; it now
describes the measured behaviour.

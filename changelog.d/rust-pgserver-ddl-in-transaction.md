### A table you have just created, in a transaction that has not ended

Creating a table and then using it, without committing in between, reported that
the relation did not exist. That is the ordinary shape of a test fixture and of
a migration, and it accounted for 184 failures in psycopg's suite by itself.

The cause is worth stating because of what hid it. Resolving a table name is
part of *planning*, planning reads the catalogue, and the catalogue is an
ordinary table — so a `CREATE TABLE` that had not committed was invisible to it.
Execution already ran inside the transaction, so anything that used a table
which already existed worked perfectly. Only the combination failed, and only
for a client that had not committed.

Fixing it exposed a second problem underneath, which had never been reachable:
`COPY` inside a transaction wrote its rows outside that transaction, where they
blocked against its own locks and hung the connection outright. Rows from a
`COPY` now go through the transaction like every other write.

`ORDER BY 1` also works now. It means the first output *column* — an ordinal
into the select list, so `select b, a from t order by 1` orders by `b` — rather
than the constant one, and a position with no such column gets PostgreSQL's own
error for it.

One thing this does not fix, and it is worth being plain about: rolling back
does not undo the `CREATE`. DDL is not transactional on this server, where in
PostgreSQL it is. That needs schema operations to participate in the
transaction down in the storage layer, and it is recorded as an open divergence
rather than quietly left to be discovered.

#### Fixed

- A table created in a transaction was invisible to later statements in the same
  transaction, and one dropped in a transaction was still found.
- `COPY` inside a transaction deadlocked the connection.

#### Added

- `ORDER BY <position>`, with PostgreSQL's error for a position that is not in
  the select list.

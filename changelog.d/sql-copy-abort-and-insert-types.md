### An abandoned `COPY … TO STDOUT` aborts the transaction, and `INSERT` checks its value types

A client that started a large `COPY … TO STDOUT`, read one row and gave up left
the transaction usable. PostgreSQL leaves it in the aborted state, so the next
statement gets "current transaction is aborted" — SecantusDB ran it instead.
The copy sent its whole result in one write, so there was no moment at which it
could notice the client had gone. It now sends in batches and checks for a
cancelled statement between them.

The row count is why this went unnoticed: a small copy fits in the buffer and
finishes before the client can abandon it, on PostgreSQL too. The difference
only shows on a copy large enough that the server is still sending.

`INSERT INTO t (col) VALUES (…)` also now rejects a value PostgreSQL would not
assign, the way `UPDATE … SET` already did — `INSERT INTO t (jsonb_col) VALUES
(42)` is an error rather than a silent coercion. `json` and `jsonb` columns are
type-checked at all for the first time, so comparing one against text is now
reported instead of quietly answering false.

#### Fixed

- `COPY … TO STDOUT` is a cancellation point: abandoning one aborts the
  enclosing transaction block, as PostgreSQL does. A copy that completes still
  returns every row in text, CSV and binary formats.
- `INSERT` applies PostgreSQL's assignment-cast rules, reporting `42804` for a
  value that cannot be assigned to the target column.
- A `json` / `jsonb` column compared against text or a number now reports
  `operator does not exist`, matching PostgreSQL, instead of silently
  evaluating to false.

#### Notes

Three long-standing entries turned out to be **already fixed** and were closed
by measurement rather than code: sub-millisecond timestamps round-trip exactly
(200 random values, zero loss) since the companion-field work; query-pipeline
aborts already discard the rest of the batch and match PostgreSQL's statuses
and rows exactly; and index changes are already undone by `ROLLBACK TO
SAVEPOINT`.

`numeric` beyond 34 significant digits was re-checked and **does** still round
— that one is a real, permanent storage ceiling, already documented as such.

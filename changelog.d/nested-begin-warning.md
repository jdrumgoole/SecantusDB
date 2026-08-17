### Nested BEGIN warns like PostgreSQL

Issuing BEGIN inside an already-open transaction block now completes with
the BEGIN tag while emitting PostgreSQL's exact warning — a NoticeResponse
with severity WARNING, SQLSTATE 25001, "there is already a transaction in
progress", and PG's source-identity fields (File xact.c, Routine
BeginTransactionBlock) — and the open block survives untouched. The notice
plumbing gained optional sqlstate/file/routine fields along the way. The
pgtest `implicit_txn` corpus file reads the warning's fields byte-for-byte
and is now green.

#### Fixed
- A nested BEGIN in an explicit block completed silently, with no warning.

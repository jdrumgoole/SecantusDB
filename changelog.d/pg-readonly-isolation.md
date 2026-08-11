### Read-only transactions are enforced; isolation level round-trips

Writes inside a read-only transaction now fail with PostgreSQL's 25006
(`cannot execute INSERT in a read-only transaction`) — whether the
read-only-ness came from `BEGIN READ ONLY`, `SET TRANSACTION READ
ONLY`, or `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY`. DML,
DDL, TRUNCATE, MERGE, and GRANT are gated; reads are untouched. And
`SHOW TRANSACTION ISOLATION LEVEL` — the multi-word spelling pgjdbc's
`getTransactionIsolation` issues verbatim (previously resolving to an
unknown GUC and an empty string) — now reports the level a
`SET SESSION CHARACTERISTICS` planted, as does `SHOW TIME ZONE`.

pgjdbc: ConnectionTest 15/15 (was 12/15), DatabaseMetaData
TransactionIsolationTest 14/14 (was 8/14), AutoSaveTransactionSettings
4/6 (was 0/6). Known divergences, none gauge-exercised, recorded in
`tasks/backlog.md`: temp-table writes are also blocked (PG allows them
under read-only), and `SELECT … FOR UPDATE` / `nextval()` are not yet
gated (PG blocks both).

### PostgreSQL Large Object API over Fastpath

The PG server now implements PostgreSQL's Large Object surface the way
pgjdbc's `LargeObjectManager` (and therefore JDBC `Blob`/`Clob`) drives it:
the Fastpath sub-protocol ('F' FunctionCall / 'V' FunctionCallResponse)
dispatching `lo_open` / `lo_close` / `loread` / `lowrite` / `lo_lseek` /
`lo_creat` / `lo_create` / `lo_tell` / `lo_unlink` / `lo_truncate` and their
64-bit variants by their real `pg_proc` OIDs, reflected into
`pg_catalog.pg_proc` so drivers can resolve them by name. Object bytes live
in chunked, sparse per-database collections (a 2GB `lo_truncate` extension
stores nothing and reads back as zeros, like PG's own representation), and
reads/writes join the session's open transaction so `ROLLBACK` discards
`lowrite` data. `lo_creat` / `lo_create` / `lo_unlink` are also SQL-callable.

Around it, the pieces pgjdbc's CallableStatement and Blob tests need: a
user-defined function call in FROM position (`select * from f($1) as
result`, pgjdbc's rewrite of `{? = call f(?)}`) evaluates as a one-row
source typed by the function's declared return type; extended-protocol
Describe derives that shape from the catalog **without executing the
function body** (a side-effecting UDF in a pgjdbc batch previously ran
twice — once at Describe, once at Execute); a NULL parameter declared
`void` (oid 2278) is dropped from the call's argument list, matching PG's
accommodation of the JDBC OUT-parameter slot; plpgsql gains `RAISE`
(NOTICE/WARNING/etc. flow to the wire as NoticeResponse, EXCEPTION raises
`P0001`); and contrib/lo's `lo_manage` trigger DDL is accepted as a
recognized no-op. pgjdbc's `BlobTest` (28), `BlobTransactionTest`,
`CallableStmtTest` (14), and `CleanupSavepointsWithFastpathTest` (10) all
pass fully — all four were previously zeroed.

#### Added
- `secantus/sql/largeobjects.py`: chunked sparse LO store + Fastpath
  dispatch with PG's real `pg_proc` OIDs; per-session descriptors.
- Fastpath sub-protocol handling in the PG wire server
  (`parse_function_call` / `function_call_response`).
- plpgsql `RAISE` statement (levels, `%` formatting, notice delivery over
  both simple and extended protocol).
- UDF and built-in function calls (`now()`, `version()`) as one-row FROM
  sources, typed by declared return type.

#### Fixed
- Extended-protocol Describe no longer executes side-effecting UDF bodies
  to derive the result shape (pgjdbc batched `{call f(?)}` ran every
  insert twice).
- `Storage.use_user_transaction` is re-entrant (nested entry from a
  SQL-callable `lo_creat` inside a transactional INSERT no longer breaks
  the outer transaction).

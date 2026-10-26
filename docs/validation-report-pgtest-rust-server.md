# pgtest wire-protocol conformance report

- SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17
- corpus + runner: cockroachdb/cockroach @ `e3bff5d92ac1` (`pkg/sql/pgwire/testdata/pgtest`, run by `pkg/testutils/pgtest` verbatim)
- generated: 2026-10-26 06:47 UTC

**23/57 files pass** (11 expected divergences, 18 unexpected failures, 5 skipped).

| file | result |
|---|---|
| `aborted_txn` | **FAIL** |
| `array` | pass |
| `as_of_system_time` | skip |
| `batch_stmt` | **FAIL** |
| `bind_and_resolve` | **FAIL** |
| `box2d` | expected divergence |
| `char` | expected divergence |
| `citext` | **FAIL** |
| `collated_string` | **FAIL** |
| `copy` | **FAIL** |
| `copy_file_upload` | pass |
| `data_type_size` | pass |
| `decimal` | **FAIL** |
| `enum` | pass |
| `errors` | pass |
| `execute` | pass |
| `float` | pass |
| `implicit_txn` | **FAIL** |
| `inet` | pass |
| `int2vector` | expected divergence |
| `int_size` | pass |
| `json` | pass |
| `json_array` | pass |
| `jsonpath` | expected divergence |
| `large_input` | pass |
| `ltree` | **FAIL** |
| `multiple_active_portals` | expected divergence |
| `multiple_active_portals/bind_to_an_existing_active_portal` | **FAIL** |
| `multiple_active_portals/select_from_individual_resources` | pass |
| `multiple_active_portals/select_from_same_table` | pass |
| `notice` | pass |
| `oid` | pass |
| `param_status` | **FAIL** |
| `parameter_description` | **FAIL** |
| `pgjdbc` | **FAIL** |
| `pgvector` | expected divergence |
| `portals` | expected divergence |
| `portals_crbugs` | skip |
| `prepare` | **FAIL** |
| `prepared_stmt_invalidation` | pass |
| `procedure` | expected divergence |
| `read_committed` | skip |
| `row_description` | expected divergence |
| `schema_changes_implicit_txn` | **FAIL** |
| `set` | **FAIL** |
| `set_transaction` | skip |
| `show_commit_timestamp` | skip |
| `simple` | pass |
| `spatial` | expected divergence |
| `statement_hints_pausable_portal` | pass |
| `timezone` | pass |
| `tuple` | **FAIL** |
| `typing` | expected divergence |
| `unknown` | **FAIL** |
| `update_limit` | pass |
| `varbit` | pass |
| `void` | pass |

## Expected divergences

- `box2d` — PostGIS BOX2D, an extension type outside SecantusDB's core-PostgreSQL scope. Both servers stop at box2d:9 with 42704 `type "box2d" does not exist`, as PostgreSQL does without PostGIS.
- `char` — PostgreSQL 15.19 stops at char:250, which pins TableOID=105 (crdb's descriptor id) with no ignore_table_oids directive. The Rust server stops earlier, at :201: `SELECT 0::"char"` in binary comes back NULL instead of the byte 00 (RP-085).
- `int2vector` — Both servers stop at int2vector:10: the stanza expects no notice for `DROP TABLE IF EXISTS t` on a missing table, and PostgreSQL 15.19 and the Rust server both send NOTICE `table "t" does not exist, skipping`. The indoption stanza at :26 (crdb's NULLS-FIRST {2} for a plain primary key; PostgreSQL reports 0) is not reached.
- `jsonpath` — Both servers stop at jsonpath:5, `SELECT ''::JSONPATH`, where the stanza expects 42601. PostgreSQL 15.19 raises 22P02; the Rust server accepts the empty path with no error (RP-088). The binary-form stanzas at :36 / :76 (crdb's single-quoted jsonpath_send) are not reached.
- `multiple_active_portals` — A CockroachDB pausable-portal test, which PostgreSQL 15.19 fails too: `query_timeout` expects a portal paged MaxRows:1 to emit N DataRows and THEN a 57014 statement-timeout on the next pull, and `interleave_with_unpausable_portal` pins crdb's `0A000 the statement for a pausable portal must be a read-only SELECT`, which real PostgreSQL never raises. The Rust server ALSO fails `bind_to_an_existing_active_portal` (:137): binding to a portal name already open is accepted instead of 42P03 (RP-089).
- `pgvector` — The pgvector VECTOR type, an extension outside SecantusDB's core-PostgreSQL scope. Both servers stop at pgvector:15 with 42704 `type "vector" does not exist`, as PostgreSQL does without the extension.
- `portals` — Both servers stop at portals:24, an Execute of the unnamed portal with nothing bound: 34000 on both, but the stanza pins crdb's message `unknown portal ""` where PostgreSQL 15.19 says `portal "" does not exist`. The Rust server's message names an internal portal instead (`portal "POSTGRESQL_DEFAULT_NAME" does not exist`, RP-091). Nothing after :24 is exercised.
- `procedure` — Both servers stop at procedure:66, the NoticeResponse of a plpgsql `RAISE NOTICE`: the stanza pins crdb's source-location fields (`File:builtins.go, Routine:func401`), which no other server can match. PostgreSQL 15.19 sends `pl_exec.c` / `exec_stmt_raise`; the Rust server leaves both empty rather than name a C source it does not have. The messages themselves (foo / bar / baz, SQLSTATE 00000) match.
- `row_description` — PostgreSQL 15.19 stops at row_description:380, `SELECT 'foo'::STRING, 'bar'::STRING(2)`: crdb's STRING alias, which PostgreSQL rejects with 42704 `type "string" does not exist`. The Rust server stops earlier, at :125: a view's columns report attnum 0 instead of their base columns' 1 and 2 (RP-087).
- `spatial` — PostGIS GEOMETRY/GEOGRAPHY, an extension type outside SecantusDB's core-PostgreSQL scope. Both servers stop at spatial:8, a Parse of `SELECT $1::GEOMETRY`. PostgreSQL 15.19 without PostGIS fails the Parse with 42704 `type "geometry" does not exist`; the Rust server accepts the Parse and fails the binary parameter with 22021 (RP-090).
- `typing` — Both servers stop at typing:74, a Parse of `SELECT id FROM t WHERE v = $1` with `$1` declared uuid against a varchar column: the stanza pins crdb's 22023 `unsupported comparison operator`. PostgreSQL 15.19 fails the Parse with 42883 `operator does not exist: character varying = uuid`; the Rust server sends the same 42883, but only at Execute, after ParseComplete and BindComplete (RP-080).

## Unexpected failures

- `aborted_txn`
- `batch_stmt`
- `bind_and_resolve`
- `citext`
- `collated_string`
- `copy`
- `decimal`
- `implicit_txn`
- `ltree`
- `multiple_active_portals/bind_to_an_existing_active_portal`
- `param_status`
- `parameter_description`
- `pgjdbc`
- `prepare`
- `schema_changes_implicit_txn`
- `set`
- `tuple`
- `unknown`

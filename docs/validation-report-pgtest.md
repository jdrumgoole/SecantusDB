# pgtest wire-protocol conformance report

- SecantusDB (Python server) 0.6.0b11
- corpus + runner: cockroachdb/cockroach @ `e3bff5d92ac1` (`pkg/sql/pgwire/testdata/pgtest`, run by `pkg/testutils/pgtest` verbatim)
- generated: 2026-08-17 06:20 UTC

**27/58 files pass** (3 expected divergences, 23 unexpected failures, 5 skipped).

| file | result |
|---|---|
| `aborted_txn` | pass |
| `array` | pass |
| `as_of_system_time` | skip |
| `batch_stmt` | pass |
| `bind_and_resolve` | pass |
| `box2d` | **FAIL** |
| `char` | expected divergence |
| `citext` | pass |
| `collated_string` | pass |
| `copy` | pass |
| `copy_file_upload` | pass |
| `data_type_size` | pass |
| `decimal` | pass |
| `enum` | pass |
| `errors` | pass |
| `execute` | pass |
| `float` | pass |
| `implicit_txn` | pass |
| `inet` | pass |
| `int2vector` | expected divergence |
| `int_size` | pass |
| `json` | pass |
| `json_array` | pass |
| `jsonpath` | expected divergence |
| `large_input` | pass |
| `ltree` | pass |
| `multiple_active_portals` | **FAIL** |
| `multiple_active_portals/select_from_individual_resources` | pass |
| `multiple_active_portals/select_from_same_table` | **FAIL** |
| `notice` | pass |
| `oid` | **FAIL** |
| `param_status` | **FAIL** |
| `parameter_description` | **FAIL** |
| `pgjdbc` | **FAIL** |
| `pgvector` | **FAIL** |
| `portals` | **FAIL** |
| `portals_crbugs` | skip |
| `prepare` | **FAIL** |
| `prepared_stmt_invalidation` | **FAIL** |
| `procedure` | **FAIL** |
| `read_committed` | skip |
| `row_description` | **FAIL** |
| `schema_changes_implicit_txn` | **FAIL** |
| `schema_changes_implicit_txn/autocommit_for_bind` | pass |
| `schema_changes_implicit_txn/triggers` | **FAIL** |
| `set` | **FAIL** |
| `set_transaction` | skip |
| `show_commit_timestamp` | skip |
| `simple` | pass |
| `spatial` | **FAIL** |
| `statement_hints_pausable_portal` | pass |
| `timezone` | **FAIL** |
| `tuple` | **FAIL** |
| `typing` | **FAIL** |
| `unknown` | **FAIL** |
| `update_limit` | pass |
| `varbit` | **FAIL** |
| `void` | **FAIL** |

## Expected divergences

- `char` — char:250 pins TableOID=105 — crdb's deterministic descriptor id, with no ignore_table_oids directive on that stanza. Real PostgreSQL reports its own pg_class oid there too (installation-specific), so the stanza can't pass against any non-crdb server. Everything else in the file is green (oid-18 "char": casts, columns, params, 1-char truncation, NULL for empty/zero-byte, binary format).
- `int2vector` — int2vector:26 expects indoption={2} for a plain primary key — crdb's NULLS-FIRST pkey representation. Real PostgreSQL reports 0 (ASC, NULLS LAST), and so do we; matching crdb's 2 would corrupt SQLAlchemy index reflection. The BINARY int2vector encoding the stanza actually regression-tests (int2 array elements, elemoid 21 — crdb #111907 shipped int8 once) is implemented and correct.
- `jsonpath` — jsonpath:36/:76 expect crdb's BINARY jsonpath form — version byte + the SINGLE-QUOTED text ('$' -> 01272427). Real PostgreSQL's jsonpath_send emits the version byte + the canonical text WITHOUT outer quotes (0124), which is what we send. Everything else in the file is green (oid 4072, canonical $."abc" text, 42601 on an empty path, jsonb_path_query).

## Unexpected failures

- `box2d`
- `multiple_active_portals`
- `multiple_active_portals/select_from_same_table`
- `oid`
- `param_status`
- `parameter_description`
- `pgjdbc`
- `pgvector`
- `portals`
- `prepare`
- `prepared_stmt_invalidation`
- `procedure`
- `row_description`
- `schema_changes_implicit_txn`
- `schema_changes_implicit_txn/triggers`
- `set`
- `spatial`
- `timezone`
- `tuple`
- `typing`
- `unknown`
- `varbit`
- `void`

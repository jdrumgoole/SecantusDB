# pgtest wire-protocol conformance report

- SecantusDB (Python server) 0.6.0b9
- corpus + runner: cockroachdb/cockroach @ `e3bff5d92ac1` (`pkg/sql/pgwire/testdata/pgtest`, run by `pkg/testutils/pgtest` verbatim)
- generated: 2026-08-03 07:04 UTC

**8/58 files pass** (0 expected divergences, 45 unexpected failures, 5 skipped).

| file | result |
|---|---|
| `aborted_txn` | **FAIL** |
| `array` | **FAIL** |
| `as_of_system_time` | skip |
| `batch_stmt` | **FAIL** |
| `bind_and_resolve` | **FAIL** |
| `box2d` | **FAIL** |
| `char` | **FAIL** |
| `citext` | **FAIL** |
| `collated_string` | **FAIL** |
| `copy` | **FAIL** |
| `copy_file_upload` | **FAIL** |
| `data_type_size` | pass |
| `decimal` | **FAIL** |
| `enum` | **FAIL** |
| `errors` | **FAIL** |
| `execute` | **FAIL** |
| `float` | **FAIL** |
| `implicit_txn` | **FAIL** |
| `inet` | **FAIL** |
| `int2vector` | **FAIL** |
| `int_size` | **FAIL** |
| `json` | **FAIL** |
| `json_array` | **FAIL** |
| `jsonpath` | **FAIL** |
| `large_input` | pass |
| `ltree` | **FAIL** |
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

## Unexpected failures

- `aborted_txn`
- `array`
- `batch_stmt`
- `bind_and_resolve`
- `box2d`
- `char`
- `citext`
- `collated_string`
- `copy`
- `copy_file_upload`
- `decimal`
- `enum`
- `errors`
- `execute`
- `float`
- `implicit_txn`
- `inet`
- `int2vector`
- `int_size`
- `json`
- `json_array`
- `jsonpath`
- `ltree`
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

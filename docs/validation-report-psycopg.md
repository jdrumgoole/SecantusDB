# psycopg conformance report

- SecantusDB (Python server) 0.5.4b237
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-07-19 15:46 UTC

| category | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| test_adapt.py | 50 | 9 | 0 | 59 | 84.7% |
| test_capabilities.py | 12 | 0 | 9 | 21 | 100.0% |
| test_column.py | 53 | 0 | 0 | 53 | 100.0% |
| test_connection.py | 93 | 9 | 2 | 104 | 91.2% |
| test_connection_info.py | 32 | 5 | 3 | 40 | 86.5% |
| test_conninfo.py | 38 | 0 | 0 | 38 | 100.0% |
| test_copy.py | 104 | 7 | 1 | 112 | 93.7% |
| test_cursor.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_client.py | 28 | 0 | 0 | 28 | 100.0% |
| test_cursor_common.py | 280 | 0 | 8 | 288 | 100.0% |
| test_cursor_raw.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_server.py | 89 | 21 | 0 | 110 | 80.9% |
| test_encodings.py | 17 | 0 | 0 | 17 | 100.0% |
| test_errors.py | 21 | 10 | 0 | 31 | 67.7% |
| test_generators.py | 3 | 2 | 1 | 6 | 60.0% |
| test_prepared.py | 21 | 9 | 1 | 31 | 70.0% |
| test_psycopg_dbapi20.py | 70 | 10 | 0 | 80 | 87.5% |
| test_query.py | 44 | 0 | 0 | 44 | 100.0% |
| test_rows.py | 18 | 0 | 0 | 18 | 100.0% |
| test_sql.py | 119 | 0 | 5 | 124 | 100.0% |
| test_transaction.py | 83 | 1 | 1 | 85 | 98.8% |
| test_typeinfo.py | 85 | 1 | 0 | 86 | 98.8% |
| test_typing.py | 125 | 0 | 0 | 125 | 100.0% |
| types/test_array.py | 158 | 0 | 0 | 158 | 100.0% |
| types/test_bool.py | 15 | 0 | 0 | 15 | 100.0% |
| types/test_composite.py | 79 | 0 | 0 | 79 | 100.0% |
| types/test_datetime.py | 558 | 0 | 9 | 567 | 100.0% |
| types/test_enum.py | 197 | 0 | 0 | 197 | 100.0% |
| types/test_hstore.py | 24 | 0 | 15 | 39 | 100.0% |
| types/test_json.py | 258 | 0 | 0 | 258 | 100.0% |
| types/test_multirange.py | 205 | 0 | 12 | 217 | 100.0% |
| types/test_net.py | 33 | 0 | 0 | 33 | 100.0% |
| types/test_none.py | 1 | 0 | 0 | 1 | 100.0% |
| types/test_numeric.py | 369 | 1 | 0 | 370 | 99.7% |
| types/test_numpy.py | 166 | 0 | 6 | 172 | 100.0% |
| types/test_range.py | 275 | 0 | 12 | 287 | 100.0% |
| types/test_shapely.py | 2 | 0 | 26 | 28 | 100.0% |
| types/test_string.py | 134 | 0 | 1 | 135 | 100.0% |
| types/test_uuid.py | 26 | 0 | 0 | 26 | 100.0% |
| **total** | **4041** | **85** | **112** | **4238** | **97.9%** |

## Failures (85)

- `tests/test_adapt.py::test_no_cast_needed[b]`
- `tests/test_adapt.py::test_no_cast_needed[s]`
- `tests/test_adapt.py::test_no_cast_needed[t]`
- `tests/test_adapt.py::test_random[0-b]`
- `tests/test_adapt.py::test_random[0-s]`
- `tests/test_adapt.py::test_random[1-b]`
- `tests/test_adapt.py::test_random[1-s]`
- `tests/test_adapt.py::test_random[1-t]`
- `tests/test_adapt.py::test_return_untyped[b]`
- `tests/test_connection.py::test_broken`
- `tests/test_connection.py::test_broken_connection`
- `tests/test_connection.py::test_cancel_safe_error`
- `tests/test_connection.py::test_cancel_safe_timeout`
- `tests/test_connection.py::test_connect_bad`
- `tests/test_connection.py::test_context_inerror_rollback_no_clobber`
- `tests/test_connection.py::test_notice_handlers`
- `tests/test_connection.py::test_right_exception_on_server_disconnect`
- `tests/test_connection.py::test_right_exception_on_session_timeout`
- `tests/test_connection_info.py::test_encoding_env_var[euc-jp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_encoding_env_var[eucjp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_normalize_encoding[euc-jp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_normalize_encoding[eucjp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_set_encoding_unsupported`
- `tests/test_copy.py::test_copy_from_leaks[0-False]`
- `tests/test_copy.py::test_copy_from_leaks[0-True]`
- `tests/test_copy.py::test_copy_from_leaks[1-True]`
- `tests/test_copy.py::test_copy_out_error_with_copy_not_finished`
- `tests/test_copy.py::test_copy_table_across[binary]`
- `tests/test_copy.py::test_copy_table_across[block]`
- `tests/test_copy.py::test_copy_table_across[row]`
- `tests/test_cursor_server.py::test_binary_cursor_execute[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_binary_cursor_execute[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_binary_cursor_text_override[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_binary_cursor_text_override[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_close[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_description[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_description[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_execute_binary[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_execute_binary[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_execute_error[asyncio-RawServerCursor-create table ssc ()]`
- `tests/test_cursor_server.py::test_execute_error[asyncio-RawServerCursor-wat]`
- `tests/test_cursor_server.py::test_execute_error[asyncio-ServerCursor-create table ssc ()]`
- `tests/test_cursor_server.py::test_execute_error[asyncio-ServerCursor-select 1; select 2]`
- `tests/test_cursor_server.py::test_execute_error[asyncio-ServerCursor-wat]`
- `tests/test_cursor_server.py::test_hold[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_non_scrollable[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_non_scrollable[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_row_factory[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_row_factory[asyncio-ServerCursor]`
- `tests/test_cursor_server.py::test_scrollable[asyncio-RawServerCursor]`
- `tests/test_cursor_server.py::test_scrollable[asyncio-ServerCursor]`
- `tests/test_errors.py::test_diag_attr_values`
- `tests/test_errors.py::test_diag_encoding[latin9]`
- `tests/test_errors.py::test_diag_encoding[utf8]`
- `tests/test_errors.py::test_diag_independent`
- `tests/test_errors.py::test_error_encoding[latin9]`
- `tests/test_errors.py::test_error_encoding[utf8]`
- `tests/test_errors.py::test_pgconn_error`
- `tests/test_errors.py::test_pgconn_error_pickle`
- `tests/test_errors.py::test_query_context`
- `tests/test_errors.py::test_unknown_sqlstate`
- `tests/test_generators.py::test_cancel`
- `tests/test_generators.py::test_pipeline_communicate_abort`
- `tests/test_prepared.py::test_change_type`
- `tests/test_prepared.py::test_change_type_savepoint`
- `tests/test_prepared.py::test_different_types`
- `tests/test_prepared.py::test_evict_lru`
- `tests/test_prepared.py::test_evict_lru_deallocate`
- `tests/test_prepared.py::test_misc_statement[notify foo, 'bar']`
- `tests/test_prepared.py::test_no_prepare_multi_with_drop`
- `tests/test_prepared.py::test_params_types`
- `tests/test_prepared.py::test_untyped_json`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_commit_in_tpc_fails`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_rollback_in_tpc_fails`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_begin`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_begin_in_tpc_transaction_fails`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_begin_in_transaction_fails`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_commit_with_prepare`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_commit_without_prepare`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_rollback_with_prepare`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_tpc_rollback_without_prepare`
- `tests/test_psycopg_dbapi20.py::PsycopgTPCTests::test_xid`
- `tests/test_transaction.py::test_context_inerror_rollback_no_clobber[asyncio-pipeline=off]`
- `tests/test_typeinfo.py::test_fetch_async[asyncio-sql_ascii-IDLE-text]`
- `tests/types/test_numeric.py::test_dump_numeric_exhaustive[b]`

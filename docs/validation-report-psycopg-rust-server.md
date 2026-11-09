# psycopg conformance report

- SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-11-09 07:03 UTC

| category | passed | failed | expected | skipped | total | pass rate | adjusted |
|---|---|---|---|---|---|---|---|
| pq/test_async.py | 14 | 0 | 0 | 2 | 16 | 100.0% | 100.0% |
| pq/test_conninfo.py | 3 | 0 | 0 | 1 | 4 | 100.0% | 100.0% |
| pq/test_copy.py | 8 | 0 | 0 | 0 | 8 | 100.0% | 100.0% |
| pq/test_escaping.py | 32 | 0 | 0 | 0 | 32 | 100.0% | 100.0% |
| pq/test_exec.py | 17 | 0 | 0 | 2 | 19 | 100.0% | 100.0% |
| pq/test_misc.py | 7 | 0 | 0 | 0 | 7 | 100.0% | 100.0% |
| pq/test_pgconn.py | 53 | 0 | 0 | 7 | 60 | 100.0% | 100.0% |
| pq/test_pgresult.py | 26 | 0 | 0 | 0 | 26 | 100.0% | 100.0% |
| pq/test_pipeline.py | 4 | 0 | 0 | 1 | 5 | 100.0% | 100.0% |
| pq/test_pq.py | 4 | 0 | 0 | 3 | 7 | 100.0% | 100.0% |
| test_adapt.py | 59 | 0 | 0 | 0 | 59 | 100.0% | 100.0% |
| test_capabilities.py | 12 | 0 | 0 | 9 | 21 | 100.0% | 100.0% |
| test_column.py | 53 | 0 | 0 | 0 | 53 | 100.0% | 100.0% |
| test_concurrency.py | 16 | 0 | 0 | 0 | 16 | 100.0% | 100.0% |
| test_concurrency_async.py | 12 | 1 | 0 | 0 | 13 | 92.3% | 92.3% |
| test_connection.py | 102 | 0 | 0 | 2 | 104 | 100.0% | 100.0% |
| test_connection_async.py | 103 | 0 | 0 | 5 | 108 | 100.0% | 100.0% |
| test_connection_info.py | 37 | 0 | 0 | 3 | 40 | 100.0% | 100.0% |
| test_conninfo.py | 38 | 0 | 0 | 0 | 38 | 100.0% | 100.0% |
| test_conninfo_attempts.py | 27 | 0 | 0 | 0 | 27 | 100.0% | 100.0% |
| test_conninfo_attempts_async.py | 27 | 0 | 0 | 0 | 27 | 100.0% | 100.0% |
| test_copy.py | 112 | 0 | 0 | 0 | 112 | 100.0% | 100.0% |
| test_copy_async.py | 112 | 0 | 0 | 0 | 112 | 100.0% | 100.0% |
| test_cursor.py | 78 | 0 | 0 | 0 | 78 | 100.0% | 100.0% |
| test_cursor_async.py | 78 | 0 | 0 | 0 | 78 | 100.0% | 100.0% |
| test_cursor_client.py | 28 | 0 | 0 | 0 | 28 | 100.0% | 100.0% |
| test_cursor_client_async.py | 28 | 0 | 0 | 0 | 28 | 100.0% | 100.0% |
| test_cursor_common.py | 280 | 0 | 0 | 8 | 288 | 100.0% | 100.0% |
| test_cursor_common_async.py | 280 | 0 | 0 | 8 | 288 | 100.0% | 100.0% |
| test_cursor_raw.py | 78 | 0 | 0 | 0 | 78 | 100.0% | 100.0% |
| test_cursor_raw_async.py | 78 | 0 | 0 | 0 | 78 | 100.0% | 100.0% |
| test_cursor_server.py | 110 | 0 | 0 | 0 | 110 | 100.0% | 100.0% |
| test_cursor_server_async.py | 110 | 0 | 0 | 0 | 110 | 100.0% | 100.0% |
| test_encodings.py | 17 | 0 | 0 | 0 | 17 | 100.0% | 100.0% |
| test_errors.py | 31 | 0 | 0 | 0 | 31 | 100.0% | 100.0% |
| test_generators.py | 5 | 0 | 0 | 1 | 6 | 100.0% | 100.0% |
| test_module.py | 8 | 0 | 0 | 0 | 8 | 100.0% | 100.0% |
| test_notify.py | 15 | 0 | 0 | 0 | 15 | 100.0% | 100.0% |
| test_notify_async.py | 15 | 0 | 0 | 0 | 15 | 100.0% | 100.0% |
| test_pipeline.py | 45 | 0 | 0 | 0 | 45 | 100.0% | 100.0% |
| test_pipeline_async.py | 45 | 0 | 0 | 0 | 45 | 100.0% | 100.0% |
| test_prepared.py | 30 | 0 | 0 | 1 | 31 | 100.0% | 100.0% |
| test_prepared_async.py | 30 | 0 | 0 | 1 | 31 | 100.0% | 100.0% |
| test_psycopg_dbapi20.py | 80 | 0 | 0 | 0 | 80 | 100.0% | 100.0% |
| test_query.py | 44 | 0 | 0 | 0 | 44 | 100.0% | 100.0% |
| test_rows.py | 18 | 0 | 0 | 0 | 18 | 100.0% | 100.0% |
| test_sql.py | 124 | 0 | 0 | 0 | 124 | 100.0% | 100.0% |
| test_tpc.py | 19 | 0 | 0 | 2 | 21 | 100.0% | 100.0% |
| test_tpc_async.py | 19 | 0 | 0 | 2 | 21 | 100.0% | 100.0% |
| test_transaction.py | 84 | 0 | 0 | 1 | 85 | 100.0% | 100.0% |
| test_transaction_async.py | 84 | 0 | 0 | 1 | 85 | 100.0% | 100.0% |
| test_typeinfo.py | 86 | 0 | 0 | 0 | 86 | 100.0% | 100.0% |
| test_typing.py | 125 | 0 | 0 | 0 | 125 | 100.0% | 100.0% |
| test_waiting.py | 149 | 0 | 0 | 12 | 161 | 100.0% | 100.0% |
| test_waiting_async.py | 29 | 0 | 0 | 2 | 31 | 100.0% | 100.0% |
| test_xid.py | 3 | 0 | 0 | 0 | 3 | 100.0% | 100.0% |
| types/test_array.py | 158 | 0 | 0 | 0 | 158 | 100.0% | 100.0% |
| types/test_bool.py | 15 | 0 | 0 | 0 | 15 | 100.0% | 100.0% |
| types/test_composite.py | 79 | 0 | 0 | 0 | 79 | 100.0% | 100.0% |
| types/test_datetime.py | 555 | 0 | 0 | 12 | 567 | 100.0% | 100.0% |
| types/test_enum.py | 197 | 0 | 0 | 0 | 197 | 100.0% | 100.0% |
| types/test_hstore.py | 39 | 0 | 0 | 0 | 39 | 100.0% | 100.0% |
| types/test_json.py | 258 | 0 | 0 | 0 | 258 | 100.0% | 100.0% |
| types/test_multirange.py | 205 | 0 | 0 | 12 | 217 | 100.0% | 100.0% |
| types/test_net.py | 33 | 0 | 0 | 0 | 33 | 100.0% | 100.0% |
| types/test_none.py | 1 | 0 | 0 | 0 | 1 | 100.0% | 100.0% |
| types/test_numeric.py | 370 | 0 | 0 | 0 | 370 | 100.0% | 100.0% |
| types/test_numpy.py | 166 | 0 | 0 | 6 | 172 | 100.0% | 100.0% |
| types/test_range.py | 275 | 0 | 0 | 12 | 287 | 100.0% | 100.0% |
| types/test_shapely.py | 28 | 0 | 0 | 0 | 28 | 100.0% | 100.0% |
| types/test_string.py | 134 | 0 | 0 | 1 | 135 | 100.0% | 100.0% |
| types/test_uuid.py | 26 | 0 | 0 | 0 | 26 | 100.0% | 100.0% |
| **total** | **5670** | **1** | **0** | **117** | **5788** | **99.9%** | **99.9%** |

## Failures (1)

- `tests/test_concurrency_async.py::test_type_error_shadow`

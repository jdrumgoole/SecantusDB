# psycopg conformance report

- SecantusDB (Python server) 0.6.0b16
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-09-21 06:49 UTC

| category | passed | failed | expected | skipped | total | pass rate | adjusted |
|---|---|---|---|---|---|---|---|
| pq/test_async.py | 14 | 0 | 0 | 2 | 16 | 100.0% | 100.0% |
| pq/test_conninfo.py | 3 | 0 | 0 | 1 | 4 | 100.0% | 100.0% |
| pq/test_copy.py | 8 | 0 | 0 | 0 | 8 | 100.0% | 100.0% |
| pq/test_escaping.py | 32 | 0 | 0 | 0 | 32 | 100.0% | 100.0% |
| pq/test_exec.py | 17 | 0 | 0 | 2 | 19 | 100.0% | 100.0% |
| pq/test_misc.py | 7 | 0 | 0 | 0 | 7 | 100.0% | 100.0% |
| pq/test_pgconn.py | 49 | 4 | 0 | 7 | 60 | 92.4% | 92.4% |
| pq/test_pgresult.py | 26 | 0 | 0 | 0 | 26 | 100.0% | 100.0% |
| pq/test_pipeline.py | 4 | 0 | 0 | 1 | 5 | 100.0% | 100.0% |
| pq/test_pq.py | 4 | 0 | 0 | 3 | 7 | 100.0% | 100.0% |
| test_adapt.py | 59 | 0 | 0 | 0 | 59 | 100.0% | 100.0% |
| test_capabilities.py | 12 | 0 | 0 | 9 | 21 | 100.0% | 100.0% |
| test_column.py | 53 | 0 | 0 | 0 | 53 | 100.0% | 100.0% |
| test_concurrency.py | 14 | 2 | 0 | 0 | 16 | 87.5% | 87.5% |
| test_concurrency_async.py | 11 | 2 | 0 | 0 | 13 | 84.6% | 84.6% |
| test_connection.py | 100 | 2 | 0 | 2 | 104 | 98.0% | 98.0% |
| test_connection_async.py | 101 | 2 | 0 | 5 | 108 | 98.0% | 98.0% |
| test_connection_info.py | 37 | 0 | 0 | 3 | 40 | 100.0% | 100.0% |
| test_conninfo.py | 38 | 0 | 0 | 0 | 38 | 100.0% | 100.0% |
| test_conninfo_attempts.py | 27 | 0 | 0 | 0 | 27 | 100.0% | 100.0% |
| test_conninfo_attempts_async.py | 27 | 0 | 0 | 0 | 27 | 100.0% | 100.0% |
| test_copy.py | 111 | 1 | 0 | 0 | 112 | 99.1% | 99.1% |
| test_copy_async.py | 111 | 1 | 0 | 0 | 112 | 99.1% | 99.1% |
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
| test_errors.py | 29 | 2 | 0 | 0 | 31 | 93.5% | 93.5% |
| test_generators.py | 5 | 0 | 0 | 1 | 6 | 100.0% | 100.0% |
| test_module.py | 8 | 0 | 0 | 0 | 8 | 100.0% | 100.0% |
| test_notify.py | 9 | 6 | 0 | 0 | 15 | 60.0% | 60.0% |
| test_notify_async.py | 9 | 6 | 0 | 0 | 15 | 60.0% | 60.0% |
| test_pipeline.py | 45 | 0 | 0 | 0 | 45 | 100.0% | 100.0% |
| test_pipeline_async.py | 45 | 0 | 0 | 0 | 45 | 100.0% | 100.0% |
| test_prepared.py | 30 | 0 | 0 | 1 | 31 | 100.0% | 100.0% |
| test_prepared_async.py | 30 | 0 | 0 | 1 | 31 | 100.0% | 100.0% |
| test_psycopg_dbapi20.py | 80 | 0 | 0 | 0 | 80 | 100.0% | 100.0% |
| test_query.py | 44 | 0 | 0 | 0 | 44 | 100.0% | 100.0% |
| test_rows.py | 18 | 0 | 0 | 0 | 18 | 100.0% | 100.0% |
| test_sql.py | 119 | 0 | 0 | 5 | 124 | 100.0% | 100.0% |
| test_tpc.py | 18 | 1 | 0 | 2 | 21 | 94.7% | 94.7% |
| test_tpc_async.py | 18 | 1 | 0 | 2 | 21 | 94.7% | 94.7% |
| test_transaction.py | 84 | 0 | 0 | 1 | 85 | 100.0% | 100.0% |
| test_transaction_async.py | 84 | 0 | 0 | 1 | 85 | 100.0% | 100.0% |
| test_typeinfo.py | 86 | 0 | 0 | 0 | 86 | 100.0% | 100.0% |
| test_typing.py | 125 | 0 | 0 | 0 | 125 | 100.0% | 100.0% |
| test_waiting.py | 148 | 1 | 0 | 12 | 161 | 99.3% | 99.3% |
| test_waiting_async.py | 28 | 1 | 0 | 2 | 31 | 96.5% | 96.5% |
| test_xid.py | 3 | 0 | 0 | 0 | 3 | 100.0% | 100.0% |
| types/test_array.py | 158 | 0 | 0 | 0 | 158 | 100.0% | 100.0% |
| types/test_bool.py | 15 | 0 | 0 | 0 | 15 | 100.0% | 100.0% |
| types/test_composite.py | 78 | 1 | 0 | 0 | 79 | 98.7% | 98.7% |
| types/test_datetime.py | 551 | 4 | 0 | 12 | 567 | 99.2% | 99.2% |
| types/test_enum.py | 197 | 0 | 0 | 0 | 197 | 100.0% | 100.0% |
| types/test_hstore.py | 24 | 15 | 0 | 0 | 39 | 61.5% | 61.5% |
| types/test_json.py | 237 | 21 | 0 | 0 | 258 | 91.8% | 91.8% |
| types/test_multirange.py | 205 | 0 | 0 | 12 | 217 | 100.0% | 100.0% |
| types/test_net.py | 33 | 0 | 0 | 0 | 33 | 100.0% | 100.0% |
| types/test_none.py | 1 | 0 | 0 | 0 | 1 | 100.0% | 100.0% |
| types/test_numeric.py | 370 | 0 | 0 | 0 | 370 | 100.0% | 100.0% |
| types/test_numpy.py | 157 | 9 | 0 | 6 | 172 | 94.5% | 94.5% |
| types/test_range.py | 275 | 0 | 0 | 12 | 287 | 100.0% | 100.0% |
| types/test_shapely.py | 2 | 0 | 0 | 26 | 28 | 100.0% | 100.0% |
| types/test_string.py | 134 | 0 | 0 | 1 | 135 | 100.0% | 100.0% |
| types/test_uuid.py | 26 | 0 | 0 | 0 | 26 | 100.0% | 100.0% |
| **total** | **5558** | **82** | **0** | **148** | **5788** | **98.5%** | **98.5%** |

## Failures (82)

- `tests/pq/test_pgconn.py::test_change_password`
- `tests/pq/test_pgconn.py::test_change_password_error`
- `tests/pq/test_pgconn.py::test_connect_async_bad`
- `tests/pq/test_pgconn.py::test_connectdb_error`
- `tests/test_concurrency.py::test_cancel_stream`
- `tests/test_concurrency.py::test_notifies`
- `tests/test_concurrency_async.py::test_cancel_stream[asyncio]`
- `tests/test_concurrency_async.py::test_type_error_shadow`
- `tests/test_connection.py::test_connect_bad`
- `tests/test_connection.py::test_right_exception_on_server_disconnect`
- `tests/test_connection_async.py::test_connect_bad[asyncio]`
- `tests/test_connection_async.py::test_right_exception_on_server_disconnect[asyncio]`
- `tests/test_copy.py::test_set_custom_type`
- `tests/test_copy_async.py::test_set_custom_type[asyncio]`
- `tests/test_errors.py::test_pgconn_error`
- `tests/test_errors.py::test_pgconn_error_pickle`
- `tests/test_notify.py::test_notify`
- `tests/test_notify.py::test_notify_query_notify[generator-client]`
- `tests/test_notify.py::test_notify_query_notify[generator-server]`
- `tests/test_notify.py::test_notify_timeout`
- `tests/test_notify.py::test_notify_timeout_0`
- `tests/test_notify.py::test_stop_after`
- `tests/test_notify_async.py::test_notify[asyncio]`
- `tests/test_notify_async.py::test_notify_query_notify[asyncio-generator-client]`
- `tests/test_notify_async.py::test_notify_query_notify[asyncio-generator-server]`
- `tests/test_notify_async.py::test_notify_timeout[asyncio]`
- `tests/test_notify_async.py::test_notify_timeout_0[asyncio]`
- `tests/test_notify_async.py::test_stop_after[asyncio]`
- `tests/test_tpc.py::TestTPC::test_recovered_xids`
- `tests/test_tpc_async.py::TestTPC::test_recovered_xids[asyncio]`
- `tests/test_waiting.py::test_wait_conn_bad`
- `tests/test_waiting_async.py::test_wait_conn_bad[asyncio]`
- `tests/types/test_composite.py::test_dump_recursive_composite[b]`
- `tests/types/test_datetime.py::TestDateTimeTz::test_max_with_timezone[0-max--06-America/Chicago]`
- `tests/types/test_datetime.py::TestDateTimeTz::test_max_with_timezone[0-min-+09:18:59-Asia/Tokyo]`
- `tests/types/test_datetime.py::TestDateTimeTz::test_max_with_timezone[1-max--06-America/Chicago]`
- `tests/types/test_datetime.py::TestDateTimeTz::test_max_with_timezone[1-min-+09:18:59-Asia/Tokyo]`
- `tests/types/test_hstore.py::test_register_conn[latin1]`
- `tests/types/test_hstore.py::test_register_conn[sql_ascii]`
- `tests/types/test_hstore.py::test_register_conn[utf8]`
- `tests/types/test_hstore.py::test_register_curs`
- `tests/types/test_hstore.py::test_register_globally`
- `tests/types/test_hstore.py::test_roundtrip[0-d0]`
- `tests/types/test_hstore.py::test_roundtrip[0-d1]`
- `tests/types/test_hstore.py::test_roundtrip[0-d2]`
- `tests/types/test_hstore.py::test_roundtrip[0-d3]`
- `tests/types/test_hstore.py::test_roundtrip[1-d0]`
- `tests/types/test_hstore.py::test_roundtrip[1-d1]`
- `tests/types/test_hstore.py::test_roundtrip[1-d2]`
- `tests/types/test_hstore.py::test_roundtrip[1-d3]`
- `tests/types/test_hstore.py::test_roundtrip_array[0]`
- `tests/types/test_hstore.py::test_roundtrip_array[1]`
- `tests/types/test_json.py::test_dump[b-Json-"\\u00e0\\u20ac"]`
- `tests/types/test_json.py::test_dump[b-Json-"te'xt"]`
- `tests/types/test_json.py::test_dump[b-Json-123.45]`
- `tests/types/test_json.py::test_dump[b-Json-123]`
- `tests/types/test_json.py::test_dump[b-Json-["a", 100]]`
- `tests/types/test_json.py::test_dump[b-Json-true]`
- `tests/types/test_json.py::test_dump[b-Json-{"a": 100}]`
- `tests/types/test_json.py::test_dump[s-Json-"\\u00e0\\u20ac"]`
- `tests/types/test_json.py::test_dump[s-Json-"te'xt"]`
- `tests/types/test_json.py::test_dump[s-Json-123.45]`
- `tests/types/test_json.py::test_dump[s-Json-123]`
- `tests/types/test_json.py::test_dump[s-Json-["a", 100]]`
- `tests/types/test_json.py::test_dump[s-Json-true]`
- `tests/types/test_json.py::test_dump[s-Json-{"a": 100}]`
- `tests/types/test_json.py::test_dump[t-Json-"\\u00e0\\u20ac"]`
- `tests/types/test_json.py::test_dump[t-Json-"te'xt"]`
- `tests/types/test_json.py::test_dump[t-Json-123.45]`
- `tests/types/test_json.py::test_dump[t-Json-123]`
- `tests/types/test_json.py::test_dump[t-Json-["a", 100]]`
- `tests/types/test_json.py::test_dump[t-Json-true]`
- `tests/types/test_json.py::test_dump[t-Json-{"a": 100}]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-3.1415927-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-3.1415927-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-3.1415927-float4]`

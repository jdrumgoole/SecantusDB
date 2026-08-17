# psycopg conformance report

- SecantusDB (Python server) 0.6.0b11
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-08-17 06:23 UTC

| category | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| test_adapt.py | 52 | 7 | 0 | 59 | 88.1% |
| test_capabilities.py | 12 | 0 | 9 | 21 | 100.0% |
| test_column.py | 53 | 0 | 0 | 53 | 100.0% |
| test_connection.py | 98 | 4 | 2 | 104 | 96.1% |
| test_connection_info.py | 37 | 0 | 3 | 40 | 100.0% |
| test_conninfo.py | 38 | 0 | 0 | 38 | 100.0% |
| test_copy.py | 106 | 6 | 0 | 112 | 94.6% |
| test_cursor.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_client.py | 28 | 0 | 0 | 28 | 100.0% |
| test_cursor_common.py | 280 | 0 | 8 | 288 | 100.0% |
| test_cursor_raw.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_server.py | 110 | 0 | 0 | 110 | 100.0% |
| test_encodings.py | 17 | 0 | 0 | 17 | 100.0% |
| test_errors.py | 29 | 2 | 0 | 31 | 93.5% |
| test_generators.py | 5 | 0 | 1 | 6 | 100.0% |
| test_prepared.py | 30 | 0 | 1 | 31 | 100.0% |
| test_psycopg_dbapi20.py | 80 | 0 | 0 | 80 | 100.0% |
| test_query.py | 44 | 0 | 0 | 44 | 100.0% |
| test_rows.py | 18 | 0 | 0 | 18 | 100.0% |
| test_sql.py | 119 | 0 | 5 | 124 | 100.0% |
| test_transaction.py | 84 | 0 | 1 | 85 | 100.0% |
| test_typeinfo.py | 86 | 0 | 0 | 86 | 100.0% |
| test_typing.py | 125 | 0 | 0 | 125 | 100.0% |
| types/test_array.py | 158 | 0 | 0 | 158 | 100.0% |
| types/test_bool.py | 15 | 0 | 0 | 15 | 100.0% |
| types/test_composite.py | 79 | 0 | 0 | 79 | 100.0% |
| types/test_datetime.py | 554 | 4 | 9 | 567 | 99.3% |
| types/test_enum.py | 197 | 0 | 0 | 197 | 100.0% |
| types/test_hstore.py | 24 | 15 | 0 | 39 | 61.5% |
| types/test_json.py | 240 | 18 | 0 | 258 | 93.0% |
| types/test_multirange.py | 205 | 0 | 12 | 217 | 100.0% |
| types/test_net.py | 33 | 0 | 0 | 33 | 100.0% |
| types/test_none.py | 1 | 0 | 0 | 1 | 100.0% |
| types/test_numeric.py | 370 | 0 | 0 | 370 | 100.0% |
| types/test_numpy.py | 157 | 9 | 6 | 172 | 94.6% |
| types/test_range.py | 275 | 0 | 12 | 287 | 100.0% |
| types/test_shapely.py | 2 | 0 | 26 | 28 | 100.0% |
| types/test_string.py | 134 | 0 | 1 | 135 | 100.0% |
| types/test_uuid.py | 26 | 0 | 0 | 26 | 100.0% |
| **total** | **4077** | **65** | **96** | **4238** | **98.4%** |

## Failures (65)

- `tests/test_adapt.py::test_random[0-b]`
- `tests/test_adapt.py::test_random[0-s]`
- `tests/test_adapt.py::test_random[0-t]`
- `tests/test_adapt.py::test_random[1-b]`
- `tests/test_adapt.py::test_random[1-s]`
- `tests/test_adapt.py::test_random[1-t]`
- `tests/test_adapt.py::test_return_untyped[b]`
- `tests/test_connection.py::test_cancel_safe_error`
- `tests/test_connection.py::test_cancel_safe_timeout`
- `tests/test_connection.py::test_connect_bad`
- `tests/test_connection.py::test_right_exception_on_server_disconnect`
- `tests/test_copy.py::test_copy_from_leaks[0-False]`
- `tests/test_copy.py::test_copy_out_error_with_copy_not_finished`
- `tests/test_copy.py::test_copy_table_across[binary]`
- `tests/test_copy.py::test_copy_table_across[block]`
- `tests/test_copy.py::test_copy_table_across[row]`
- `tests/test_copy.py::test_set_custom_type`
- `tests/test_errors.py::test_pgconn_error`
- `tests/test_errors.py::test_pgconn_error_pickle`
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
- `tests/types/test_json.py::test_dump[s-Json-"\\u00e0\\u20ac"]`
- `tests/types/test_json.py::test_dump[s-Json-"te'xt"]`
- `tests/types/test_json.py::test_dump[s-Json-123.45]`
- `tests/types/test_json.py::test_dump[s-Json-123]`
- `tests/types/test_json.py::test_dump[s-Json-["a", 100]]`
- `tests/types/test_json.py::test_dump[s-Json-true]`
- `tests/types/test_json.py::test_dump[t-Json-"\\u00e0\\u20ac"]`
- `tests/types/test_json.py::test_dump[t-Json-"te'xt"]`
- `tests/types/test_json.py::test_dump[t-Json-123.45]`
- `tests/types/test_json.py::test_dump[t-Json-123]`
- `tests/types/test_json.py::test_dump[t-Json-["a", 100]]`
- `tests/types/test_json.py::test_dump[t-Json-true]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[b-float32-3.1415927-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[s-float32-3.1415927-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-2.7182817-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-256e-6-float4]`
- `tests/types/test_numpy.py::test_dump_float[t-float32-3.1415927-float4]`

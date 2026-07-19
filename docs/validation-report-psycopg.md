# psycopg conformance report

- SecantusDB (Python server) 0.5.4b237
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-07-19 14:30 UTC

| category | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| test_adapt.py | 49 | 10 | 0 | 59 | 83.1% |
| test_capabilities.py | 12 | 0 | 9 | 21 | 100.0% |
| test_column.py | 53 | 0 | 0 | 53 | 100.0% |
| test_connection.py | 93 | 9 | 2 | 104 | 91.2% |
| test_connection_info.py | 32 | 5 | 3 | 40 | 86.5% |
| test_conninfo.py | 38 | 0 | 0 | 38 | 100.0% |
| test_copy.py | 105 | 6 | 1 | 112 | 94.6% |
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
| types/test_composite.py | 62 | 17 | 0 | 79 | 78.5% |
| types/test_datetime.py | 558 | 0 | 9 | 567 | 100.0% |
| types/test_enum.py | 197 | 0 | 0 | 197 | 100.0% |
| types/test_hstore.py | 24 | 0 | 15 | 39 | 100.0% |
| types/test_json.py | 258 | 0 | 0 | 258 | 100.0% |
| types/test_multirange.py | 205 | 0 | 12 | 217 | 100.0% |
| types/test_net.py | 3 | 30 | 0 | 33 | 9.1% |
| types/test_none.py | 1 | 0 | 0 | 1 | 100.0% |
| types/test_numeric.py | 366 | 4 | 0 | 370 | 98.9% |
| types/test_numpy.py | 88 | 78 | 6 | 172 | 53.0% |
| types/test_range.py | 275 | 0 | 12 | 287 | 100.0% |
| types/test_shapely.py | 2 | 0 | 26 | 28 | 100.0% |
| types/test_string.py | 134 | 0 | 1 | 135 | 100.0% |
| types/test_uuid.py | 21 | 5 | 0 | 26 | 80.8% |
| **total** | **3908** | **218** | **112** | **4238** | **94.7%** |

## Failures (218)

- `tests/test_adapt.py::test_no_cast_needed[b]`
- `tests/test_adapt.py::test_no_cast_needed[s]`
- `tests/test_adapt.py::test_no_cast_needed[t]`
- `tests/test_adapt.py::test_random[0-b]`
- `tests/test_adapt.py::test_random[0-s]`
- `tests/test_adapt.py::test_random[0-t]`
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
- `tests/types/test_composite.py::test_dump_builtin_empty_range[b]`
- `tests/types/test_composite.py::test_dump_builtin_empty_range[s]`
- `tests/types/test_composite.py::test_dump_builtin_empty_range[t]`
- `tests/types/test_composite.py::test_dump_recursive_composite[b]`
- `tests/types/test_composite.py::test_dump_tuple[-obj0]`
- `tests/types/test_composite.py::test_dump_tuple[null-obj1]`
- `tests/types/test_composite.py::test_invalid_fields_names`
- `tests/types/test_composite.py::test_load_composite[1]`
- `tests/types/test_composite.py::test_load_composite_factory[1]`
- `tests/types/test_composite.py::test_load_different_records_rows[0]`
- `tests/types/test_composite.py::test_load_different_records_rows[1]`
- `tests/types/test_composite.py::test_load_keyword_composite_factory[1]`
- `tests/types/test_composite.py::test_load_record_binary['foo''', '''foo', '"bar', 'bar"' -want5]`
- `tests/types/test_composite.py::test_load_record_binary[10::int, null::text, 20::float, null::text, 'foo'::text, 'bar'::bytea -want6]`
- `tests/types/test_composite.py::test_load_record_binary[42,'foo','ba,r','ba''z','qu"x'-want4]`
- `tests/types/test_composite.py::test_load_record_binary[null, ''-want3]`
- `tests/types/test_composite.py::test_load_recursive_composite[1]`
- `tests/types/test_net.py::test_address_dump[192.168.0.1-b]`
- `tests/types/test_net.py::test_address_dump[192.168.0.1-s]`
- `tests/types/test_net.py::test_address_dump[192.168.0.1-t]`
- `tests/types/test_net.py::test_address_dump[2001:db8::-b]`
- `tests/types/test_net.py::test_address_dump[2001:db8::-s]`
- `tests/types/test_net.py::test_address_dump[2001:db8::-t]`
- `tests/types/test_net.py::test_cidr_load[127.0.0.0/24-0]`
- `tests/types/test_net.py::test_cidr_load[127.0.0.0/24-1]`
- `tests/types/test_net.py::test_cidr_load[::ffff:102:300/128-0]`
- `tests/types/test_net.py::test_cidr_load[::ffff:102:300/128-1]`
- `tests/types/test_net.py::test_inet_load_address[127.0.0.1/32-0]`
- `tests/types/test_net.py::test_inet_load_address[127.0.0.1/32-1]`
- `tests/types/test_net.py::test_inet_load_address[::ffff:102:300/128-0]`
- `tests/types/test_net.py::test_inet_load_address[::ffff:102:300/128-1]`
- `tests/types/test_net.py::test_inet_load_network[127.0.0.1/24-0]`
- `tests/types/test_net.py::test_inet_load_network[127.0.0.1/24-1]`
- `tests/types/test_net.py::test_inet_load_network[::ffff:102:300/127-0]`
- `tests/types/test_net.py::test_inet_load_network[::ffff:102:300/127-1]`
- `tests/types/test_net.py::test_interface_dump[127.0.0.1/24-b]`
- `tests/types/test_net.py::test_interface_dump[127.0.0.1/24-s]`
- `tests/types/test_net.py::test_interface_dump[127.0.0.1/24-t]`
- `tests/types/test_net.py::test_interface_dump[::ffff:102:300/128-b]`
- `tests/types/test_net.py::test_interface_dump[::ffff:102:300/128-s]`
- `tests/types/test_net.py::test_interface_dump[::ffff:102:300/128-t]`
- `tests/types/test_net.py::test_network_dump[127.0.0.0/24-b]`
- `tests/types/test_net.py::test_network_dump[127.0.0.0/24-s]`
- `tests/types/test_net.py::test_network_dump[127.0.0.0/24-t]`
- `tests/types/test_net.py::test_network_dump[::ffff:102:300/128-b]`
- `tests/types/test_net.py::test_network_dump[::ffff:102:300/128-s]`
- `tests/types/test_net.py::test_network_dump[::ffff:102:300/128-t]`
- `tests/types/test_numeric.py::test_dump_float[b-nan-'NaN']`
- `tests/types/test_numeric.py::test_dump_float[s-nan-'NaN']`
- `tests/types/test_numeric.py::test_dump_float[t-nan-'NaN']`
- `tests/types/test_numeric.py::test_dump_numeric_exhaustive[b]`
- `tests/types/test_numpy.py::test_dump_int[b-bool_-False-'f'::bool]`
- `tests/types/test_numpy.py::test_dump_int[b-bool_-True-'t'::bool]`
- `tests/types/test_numpy.py::test_dump_int[b-int16--32768-'-32768'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-int16-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-int16-32767-'32767'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-int32--2147483648-'-2147483648'::int4]`
- `tests/types/test_numpy.py::test_dump_int[b-int32-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[b-int32-2147483647-'2147483647'::int4]`
- `tests/types/test_numpy.py::test_dump_int[b-int64--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-int64-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-int64-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-int8--128-'-128'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-int8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-int8-127-'127'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-longlong--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-longlong-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-uint16-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[b-uint16-65535-'65535'::int4]`
- `tests/types/test_numpy.py::test_dump_int[b-uint32-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-uint32-4294967295-'4294967295'::int8]`
- `tests/types/test_numpy.py::test_dump_int[b-uint64-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[b-uint64-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[b-uint8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-uint8-255-'255'::int2]`
- `tests/types/test_numpy.py::test_dump_int[b-ulonglong-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[b-ulonglong-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[s-bool_-False-'f'::bool]`
- `tests/types/test_numpy.py::test_dump_int[s-bool_-True-'t'::bool]`
- `tests/types/test_numpy.py::test_dump_int[s-int16--32768-'-32768'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-int16-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-int16-32767-'32767'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-int32--2147483648-'-2147483648'::int4]`
- `tests/types/test_numpy.py::test_dump_int[s-int32-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[s-int32-2147483647-'2147483647'::int4]`
- `tests/types/test_numpy.py::test_dump_int[s-int64--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-int64-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-int64-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-int8--128-'-128'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-int8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-int8-127-'127'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-longlong--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-longlong-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-uint16-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[s-uint16-65535-'65535'::int4]`
- `tests/types/test_numpy.py::test_dump_int[s-uint32-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-uint32-4294967295-'4294967295'::int8]`
- `tests/types/test_numpy.py::test_dump_int[s-uint64-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[s-uint64-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[s-uint8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-uint8-255-'255'::int2]`
- `tests/types/test_numpy.py::test_dump_int[s-ulonglong-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[s-ulonglong-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[t-bool_-False-'f'::bool]`
- `tests/types/test_numpy.py::test_dump_int[t-bool_-True-'t'::bool]`
- `tests/types/test_numpy.py::test_dump_int[t-int16--32768-'-32768'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-int16-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-int16-32767-'32767'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-int32--2147483648-'-2147483648'::int4]`
- `tests/types/test_numpy.py::test_dump_int[t-int32-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[t-int32-2147483647-'2147483647'::int4]`
- `tests/types/test_numpy.py::test_dump_int[t-int64--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-int64-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-int64-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-int8--128-'-128'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-int8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-int8-127-'127'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-longlong--9223372036854775808-'-9223372036854775808'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-longlong-9223372036854775807-'9223372036854775807'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-uint16-0-'0'::int4]`
- `tests/types/test_numpy.py::test_dump_int[t-uint16-65535-'65535'::int4]`
- `tests/types/test_numpy.py::test_dump_int[t-uint32-0-'0'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-uint32-4294967295-'4294967295'::int8]`
- `tests/types/test_numpy.py::test_dump_int[t-uint64-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[t-uint64-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[t-uint8-0-'0'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-uint8-255-'255'::int2]`
- `tests/types/test_numpy.py::test_dump_int[t-ulonglong-0-'0'::numeric]`
- `tests/types/test_numpy.py::test_dump_int[t-ulonglong-18446744073709551615-'18446744073709551615'::numeric]`
- `tests/types/test_uuid.py::test_uuid_dump[01234567-89ab-cdef-0123-456789abcdef-t]`
- `tests/types/test_uuid.py::test_uuid_dump[0123456789abcdef0123456789abcdef-t]`
- `tests/types/test_uuid.py::test_uuid_dump[12345678-1234-5678-1234-567812345679-t]`
- `tests/types/test_uuid.py::test_uuid_dump[12345678123456781234567812345679-t]`
- `tests/types/test_uuid.py::test_uuid_dump[{a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11}-t]`

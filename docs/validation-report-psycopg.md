# psycopg conformance report

- SecantusDB (Python server) 0.5.4b235
- psycopg suite: vendor/psycopg @ unknown
- generated: 2026-07-17 00:17 UTC

| category | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| test_adapt.py | 49 | 10 | 0 | 59 | 83.1% |
| test_capabilities.py | 12 | 0 | 9 | 21 | 100.0% |
| test_column.py | 18 | 35 | 0 | 53 | 34.0% |
| test_connection.py | 79 | 23 | 2 | 104 | 77.5% |
| test_connection_info.py | 32 | 5 | 3 | 40 | 86.5% |
| test_conninfo.py | 38 | 0 | 0 | 38 | 100.0% |
| test_copy.py | 87 | 24 | 1 | 112 | 78.4% |
| test_cursor.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_client.py | 26 | 2 | 0 | 28 | 92.9% |
| test_cursor_common.py | 277 | 3 | 8 | 288 | 98.9% |
| test_cursor_raw.py | 78 | 0 | 0 | 78 | 100.0% |
| test_cursor_server.py | 89 | 21 | 0 | 110 | 80.9% |
| test_encodings.py | 17 | 0 | 0 | 17 | 100.0% |
| test_errors.py | 21 | 10 | 0 | 31 | 67.7% |
| test_generators.py | 3 | 2 | 1 | 6 | 60.0% |
| test_prepared.py | 19 | 11 | 1 | 31 | 63.3% |
| test_psycopg_dbapi20.py | 70 | 10 | 0 | 80 | 87.5% |
| test_query.py | 44 | 0 | 0 | 44 | 100.0% |
| test_rows.py | 18 | 0 | 0 | 18 | 100.0% |
| test_sql.py | 119 | 0 | 5 | 124 | 100.0% |
| test_transaction.py | 83 | 1 | 1 | 85 | 98.8% |
| test_typeinfo.py | 85 | 1 | 0 | 86 | 98.8% |
| test_typing.py | 125 | 0 | 0 | 125 | 100.0% |
| types/test_array.py | 125 | 33 | 0 | 158 | 79.1% |
| types/test_bool.py | 15 | 0 | 0 | 15 | 100.0% |
| types/test_composite.py | 62 | 17 | 0 | 79 | 78.5% |
| types/test_datetime.py | 558 | 0 | 9 | 567 | 100.0% |
| types/test_enum.py | 197 | 0 | 0 | 197 | 100.0% |
| types/test_hstore.py | 24 | 0 | 15 | 39 | 100.0% |
| types/test_json.py | 258 | 0 | 0 | 258 | 100.0% |
| types/test_multirange.py | 203 | 2 | 12 | 217 | 99.0% |
| types/test_net.py | 3 | 30 | 0 | 33 | 9.1% |
| types/test_none.py | 1 | 0 | 0 | 1 | 100.0% |
| types/test_numeric.py | 366 | 4 | 0 | 370 | 98.9% |
| types/test_numpy.py | 88 | 78 | 6 | 172 | 53.0% |
| types/test_range.py | 264 | 11 | 12 | 287 | 96.0% |
| types/test_shapely.py | 2 | 0 | 26 | 28 | 100.0% |
| types/test_string.py | 110 | 24 | 1 | 135 | 82.1% |
| types/test_uuid.py | 21 | 5 | 0 | 26 | 80.8% |
| **total** | **3764** | **362** | **112** | **4238** | **91.2%** |

## Failures (362)

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
- `tests/test_column.py::test_description_attribs`
- `tests/test_column.py::test_details[bit(1)-None-None-1-None]`
- `tests/test_column.py::test_details[bit(42)-None-None-42-None]`
- `tests/test_column.py::test_details[bit(83886080)-None-None-83886080-None]`
- `tests/test_column.py::test_details[bpchar(42)-None-None-42-None]`
- `tests/test_column.py::test_details[numeric(1,-1000)-1--1000-None-None]`
- `tests/test_column.py::test_details[numeric(1,1000)-1-1000-None-None]`
- `tests/test_column.py::test_details[numeric(10,0)-10-0-None-None]`
- `tests/test_column.py::test_details[numeric(10,3)[]-10-3-None-None]`
- `tests/test_column.py::test_details[numeric(1000,1000)-1000-1000-None-None]`
- `tests/test_column.py::test_details[numeric(2,-3)-2--3-None-None]`
- `tests/test_column.py::test_details[numeric(3,5)-3-5-None-None]`
- `tests/test_column.py::test_details[varbit(1)-None-None-1-None]`
- `tests/test_column.py::test_details[varbit(42)-None-None-42-None]`
- `tests/test_column.py::test_details[varbit(83886080)-None-None-83886080-None]`
- `tests/test_column.py::test_details[varchar(1)-None-None-1-None]`
- `tests/test_column.py::test_details[varchar(1)[]-None-None-1-None]`
- `tests/test_column.py::test_details[varchar(10485760)-None-None-10485760-None]`
- `tests/test_column.py::test_details[varchar(42)-None-None-42-None]`
- `tests/test_column.py::test_details[varchar-None-None-None-None]`
- `tests/test_column.py::test_details_time[0-interval]`
- `tests/test_column.py::test_details_time[0-time]`
- `tests/test_column.py::test_details_time[0-timestamp]`
- `tests/test_column.py::test_details_time[0-timestamptz]`
- `tests/test_column.py::test_details_time[0-timetz]`
- `tests/test_column.py::test_details_time[2-interval]`
- `tests/test_column.py::test_details_time[2-time]`
- `tests/test_column.py::test_details_time[2-timestamp]`
- `tests/test_column.py::test_details_time[2-timestamptz]`
- `tests/test_column.py::test_details_time[2-timetz]`
- `tests/test_column.py::test_details_time[6-interval]`
- `tests/test_column.py::test_details_time[6-time]`
- `tests/test_column.py::test_details_time[6-timestamp]`
- `tests/test_column.py::test_details_time[6-timestamptz]`
- `tests/test_column.py::test_details_time[6-timetz]`
- `tests/test_connection.py::test_broken`
- `tests/test_connection.py::test_broken_connection`
- `tests/test_connection.py::test_cancel_safe_error`
- `tests/test_connection.py::test_cancel_safe_timeout`
- `tests/test_connection.py::test_connect_bad`
- `tests/test_connection.py::test_context_inerror_rollback_no_clobber`
- `tests/test_connection.py::test_notice_handlers`
- `tests/test_connection.py::test_right_exception_on_server_disconnect`
- `tests/test_connection.py::test_right_exception_on_session_timeout`
- `tests/test_connection.py::test_set_transaction_param_all`
- `tests/test_connection.py::test_set_transaction_param_all_property`
- `tests/test_connection.py::test_set_transaction_param_block[deferrable-False]`
- `tests/test_connection.py::test_set_transaction_param_block[deferrable-True]`
- `tests/test_connection.py::test_set_transaction_param_block[isolation_level-False]`
- `tests/test_connection.py::test_set_transaction_param_block[isolation_level-True]`
- `tests/test_connection.py::test_set_transaction_param_block[read_only-False]`
- `tests/test_connection.py::test_set_transaction_param_block[read_only-True]`
- `tests/test_connection.py::test_set_transaction_param_implicit[deferrable-False]`
- `tests/test_connection.py::test_set_transaction_param_implicit[isolation_level-False]`
- `tests/test_connection.py::test_set_transaction_param_implicit[read_only-False]`
- `tests/test_connection.py::test_set_transaction_param_reset[deferrable]`
- `tests/test_connection.py::test_set_transaction_param_reset[isolation_level]`
- `tests/test_connection.py::test_set_transaction_param_reset[read_only]`
- `tests/test_connection_info.py::test_encoding_env_var[euc-jp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_encoding_env_var[eucjp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_normalize_encoding[euc-jp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_normalize_encoding[eucjp-EUC_JP-euc_jp]`
- `tests/test_connection_info.py::test_set_encoding_unsupported`
- `tests/test_copy.py::test_binary_partial_row`
- `tests/test_copy.py::test_connection_writer[1-sample_binary]`
- `tests/test_copy.py::test_copy_bad_result`
- `tests/test_copy.py::test_copy_from_leaks[0-False]`
- `tests/test_copy.py::test_copy_from_leaks[0-True]`
- `tests/test_copy.py::test_copy_from_leaks[1-True]`
- `tests/test_copy.py::test_copy_in_allchars`
- `tests/test_copy.py::test_copy_in_buffers[1-sample_binary]`
- `tests/test_copy.py::test_copy_in_error`
- `tests/test_copy.py::test_copy_out_allchars[0]`
- `tests/test_copy.py::test_copy_out_allchars[1]`
- `tests/test_copy.py::test_copy_out_error_with_copy_not_finished`
- `tests/test_copy.py::test_copy_out_iter[dict_row-1]`
- `tests/test_copy.py::test_copy_out_iter[namedtuple_row-1]`
- `tests/test_copy.py::test_copy_out_iter[tuple_row-1]`
- `tests/test_copy.py::test_copy_out_read[1]`
- `tests/test_copy.py::test_copy_table_across[binary]`
- `tests/test_copy.py::test_copy_table_across[block]`
- `tests/test_copy.py::test_copy_table_across[row]`
- `tests/test_copy.py::test_read_row_notypes[1]`
- `tests/test_copy.py::test_rows_notypes[1]`
- `tests/test_copy.py::test_subclass_adapter[1]`
- `tests/test_copy.py::test_subclass_nulling_dumper[1]`
- `tests/test_copy.py::test_worker_life[1-sample_binary]`
- `tests/test_cursor_client.py::test_leak[asyncio-dict_row-many]`
- `tests/test_cursor_client.py::test_leak[asyncio-namedtuple_row-all]`
- `tests/test_cursor_common.py::test_stream_badquery[asyncio-ClientCursor-copy (select 1) to stdout]`
- `tests/test_cursor_common.py::test_stream_badquery[asyncio-Cursor-copy (select 1) to stdout]`
- `tests/test_cursor_common.py::test_stream_badquery[asyncio-RawCursor-copy (select 1) to stdout]`
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
- `tests/test_cursor_server.py::test_hold[asyncio-RawServerCursor]`
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
- `tests/test_prepared.py::test_change_type_execute`
- `tests/test_prepared.py::test_change_type_executemany`
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
- `tests/types/test_array.py::test_all_chars[1-s]`
- `tests/types/test_array.py::test_all_chars[1-t]`
- `tests/types/test_array.py::test_all_chars_with_bounds[0]`
- `tests/types/test_array.py::test_all_chars_with_bounds[1]`
- `tests/types/test_array.py::test_array_of_unknown_builtin`
- `tests/types/test_array.py::test_array_register`
- `tests/types/test_array.py::test_array_with_bounds['[0:1]={a,b}'::text[]-want0-1]`
- `tests/types/test_array.py::test_array_with_bounds['[1:1][-2:-1][3:5]={{{1,2,3},{4,5,6}}}'::int[]-want1-1]`
- `tests/types/test_array.py::test_dump_list_int[obj1-{10,null,30}]`
- `tests/types/test_array.py::test_dump_list_no_comma_separator`
- `tests/types/test_array.py::test_dump_list_str[obj0-{{{{{{a}}}}}}-b]`
- `tests/types/test_array.py::test_dump_list_str[obj2-{{{{{{"NULL"}}}}}}-b]`
- `tests/types/test_array.py::test_dump_list_str[obj4-{foo,null,baz}-b]`
- `tests/types/test_array.py::test_dump_list_str[obj4-{foo,null,baz}-s]`
- `tests/types/test_array.py::test_dump_list_str[obj4-{foo,null,baz}-t]`
- `tests/types/test_array.py::test_dump_list_str[obj6-{{foo,bar},{baz,qux},{quux,quuux}}-b]`
- `tests/types/test_array.py::test_dump_list_str[obj7-{{{"fo{o","ba}r"},{"ba\\"z",qu\\'x},{"qu ux"," "}}}-b]`
- `tests/types/test_array.py::test_dump_list_str[obj7-{{{"fo{o","ba}r"},{"ba\\"z",qu\\'x},{"qu ux"," "}}}-s]`
- `tests/types/test_array.py::test_dump_list_str[obj7-{{{"fo{o","ba}r"},{"ba\\"z",qu\\'x},{"qu ux"," "}}}-t]`
- `tests/types/test_array.py::test_empty_list[b]`
- `tests/types/test_array.py::test_empty_list[s]`
- `tests/types/test_array.py::test_empty_list[t]`
- `tests/types/test_array.py::test_load_list_int[want1-{10,null,30}-0]`
- `tests/types/test_array.py::test_load_list_int[want1-{10,null,30}-1]`
- `tests/types/test_array.py::test_load_list_int[want2-{{10,20},{30,40}}-1]`
- `tests/types/test_array.py::test_load_list_str[want0-{{{{{{a}}}}}}-1]`
- `tests/types/test_array.py::test_load_list_str[want1-{{{{{{NULL}}}}}}-1]`
- `tests/types/test_array.py::test_load_list_str[want2-{{{{{{"NULL"}}}}}}-1]`
- `tests/types/test_array.py::test_load_list_str[want4-{foo,null,baz}-0]`
- `tests/types/test_array.py::test_load_list_str[want6-{{foo,bar},{baz,qux},{quux,quuux}}-1]`
- `tests/types/test_array.py::test_load_list_str[want7-{{{"fo{o","ba}r"},{"ba\\"z",qu\\'x},{"qu ux"," "}}}-1]`
- `tests/types/test_array.py::test_load_nested_array[0]`
- `tests/types/test_array.py::test_load_nested_array[1]`
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
- `tests/types/test_multirange.py::test_dump_builtin_multirange[b-int4multirange-ranges0]`
- `tests/types/test_multirange.py::test_dump_builtin_multirange[b-int8multirange-ranges2]`
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
- `tests/types/test_range.py::test_dump_builtin_empty[b-daterange]`
- `tests/types/test_range.py::test_dump_builtin_empty[b-int4range]`
- `tests/types/test_range.py::test_dump_builtin_empty[b-int8range]`
- `tests/types/test_range.py::test_dump_builtin_empty[b-numrange]`
- `tests/types/test_range.py::test_dump_builtin_empty[b-tsrange]`
- `tests/types/test_range.py::test_dump_builtin_empty[b-tstzrange]`
- `tests/types/test_range.py::test_dump_builtin_range[b-int4range-None-None-()]`
- `tests/types/test_range.py::test_dump_builtin_range[b-int8range-None-None-()]`
- `tests/types/test_range.py::test_dump_quoting`
- `tests/types/test_range.py::test_load_quoting[0]`
- `tests/types/test_range.py::test_load_quoting[1]`
- `tests/types/test_string.py::test_dump_1byte[Binary-b]`
- `tests/types/test_string.py::test_dump_1byte[Binary-s]`
- `tests/types/test_string.py::test_dump_1byte[Binary-t]`
- `tests/types/test_string.py::test_dump_1byte[bytearray-b]`
- `tests/types/test_string.py::test_dump_1byte[bytearray-s]`
- `tests/types/test_string.py::test_dump_1byte[bytearray-t]`
- `tests/types/test_string.py::test_dump_1byte[bytes-b]`
- `tests/types/test_string.py::test_dump_1byte[bytes-s]`
- `tests/types/test_string.py::test_dump_1byte[bytes-t]`
- `tests/types/test_string.py::test_dump_1byte[memoryview-b]`
- `tests/types/test_string.py::test_dump_1byte[memoryview-s]`
- `tests/types/test_string.py::test_dump_1byte[memoryview-t]`
- `tests/types/test_string.py::test_dump_text_oid[s]`
- `tests/types/test_string.py::test_dump_text_oid[t]`
- `tests/types/test_string.py::test_quote_1byte[Binary-off]`
- `tests/types/test_string.py::test_quote_1byte[bytearray-off]`
- `tests/types/test_string.py::test_quote_1byte[bytes-off]`
- `tests/types/test_string.py::test_quote_1byte[memoryview-off]`
- `tests/types/test_string.py::test_text_array[name-0-b]`
- `tests/types/test_string.py::test_text_array[name-0-s]`
- `tests/types/test_string.py::test_text_array[name-0-t]`
- `tests/types/test_string.py::test_text_array[name-1-b]`
- `tests/types/test_string.py::test_text_array[name-1-s]`
- `tests/types/test_string.py::test_text_array[name-1-t]`
- `tests/types/test_uuid.py::test_uuid_dump[01234567-89ab-cdef-0123-456789abcdef-t]`
- `tests/types/test_uuid.py::test_uuid_dump[0123456789abcdef0123456789abcdef-t]`
- `tests/types/test_uuid.py::test_uuid_dump[12345678-1234-5678-1234-567812345679-t]`
- `tests/types/test_uuid.py::test_uuid_dump[12345678123456781234567812345679-t]`
- `tests/types/test_uuid.py::test_uuid_dump[{a0eebc99-9c0b4ef8-bb6d6bb9-bd380a11}-t]`

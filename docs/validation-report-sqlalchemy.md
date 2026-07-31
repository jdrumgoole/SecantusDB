# SQLAlchemy dialect-compliance report

- SecantusDB (Python server) 0.6.0b6
- suite: sqlalchemy.testing.suite @ SQLAlchemy 2.0.51, postgresql+psycopg dialect
- generated: 2026-07-31 13:41 UTC

| suite class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| ArgSignatureTest | 22 | 0 | 0 | 22 | 100.0% |
| BinaryTest | 3 | 0 | 0 | 3 | 100.0% |
| BitwiseTest | 0 | 0 | 6 | 6 | — |
| BizarroCharacterTest | 32 | 0 | 32 | 64 | 100.0% |
| BooleanTest | 4 | 0 | 0 | 4 | 100.0% |
| CastTypeDecoratorTest | 1 | 0 | 0 | 1 | 100.0% |
| CollateTest | 0 | 0 | 1 | 1 | — |
| ComponentReflectionTest | 257 | 0 | 492 | 749 | 100.0% |
| ComponentReflectionTestExtra | 5 | 1 | 34 | 40 | 83.3% |
| CompositeKeyReflectionTest | 2 | 0 | 0 | 2 | 100.0% |
| CompoundSelectTest | 3 | 4 | 0 | 7 | 42.9% |
| DateTest | 5 | 0 | 1 | 6 | 100.0% |
| DateTimeCoercedToDateTimeTest | 5 | 0 | 1 | 6 | 100.0% |
| DateTimeMicrosecondsTest | 2 | 2 | 1 | 5 | 50.0% |
| DateTimeTest | 5 | 0 | 1 | 6 | 100.0% |
| DeprecatedCompoundSelectTest | 3 | 2 | 0 | 5 | 60.0% |
| DifficultParametersTest | 63 | 0 | 0 | 63 | 100.0% |
| DistinctOnTest | 0 | 1 | 0 | 1 | 0.0% |
| EnumTest | 7 | 0 | 0 | 7 | 100.0% |
| EscapingTest | 1 | 0 | 0 | 1 | 100.0% |
| ExceptionTest | 2 | 0 | 0 | 2 | 100.0% |
| ExistsTest | 0 | 2 | 0 | 2 | 0.0% |
| ExpandingBoundInTest | 18 | 0 | 11 | 29 | 100.0% |
| FetchLimitOffsetTest | 15 | 1 | 13 | 29 | 93.8% |
| FutureTableDDLTest | 3 | 0 | 7 | 10 | 100.0% |
| HasIndexTest | 2 | 0 | 2 | 4 | 100.0% |
| HasSequenceTest | 5 | 0 | 6 | 11 | 100.0% |
| HasSequenceTestEmpty | 1 | 0 | 0 | 1 | 100.0% |
| HasTableTest | 4 | 0 | 4 | 8 | 100.0% |
| IdentityAutoincrementTest | 1 | 0 | 0 | 1 | 100.0% |
| InsertBehaviorTest | 11 | 1 | 0 | 12 | 91.7% |
| IntegerTest | 23 | 0 | 0 | 23 | 100.0% |
| IsOrIsNotDistinctFromTest | 5 | 0 | 0 | 5 | 100.0% |
| JoinTest | 5 | 0 | 0 | 5 | 100.0% |
| LastrowidTest | 2 | 0 | 1 | 3 | 100.0% |
| LikeFunctionsTest | 14 | 0 | 9 | 23 | 100.0% |
| LongNameBlowoutTest | 4 | 0 | 1 | 5 | 100.0% |
| NumericTest | 18 | 0 | 3 | 21 | 100.0% |
| OrderByLabelTest | 6 | 0 | 0 | 6 | 100.0% |
| PingTest | 1 | 0 | 0 | 1 | 100.0% |
| PostCompileParamsTest | 4 | 0 | 2 | 6 | 100.0% |
| QuotedNameArgumentTest | 14 | 0 | 4 | 18 | 100.0% |
| ReturningGuardsTest | 6 | 0 | 0 | 6 | 100.0% |
| ReturningTest | 19 | 4 | 34 | 57 | 82.6% |
| RowCountTest | 20 | 0 | 0 | 20 | 100.0% |
| RowFetchTest | 5 | 1 | 0 | 6 | 83.3% |
| SequenceCompilerTest | 1 | 0 | 0 | 1 | 100.0% |
| SequenceTest | 3 | 2 | 3 | 8 | 60.0% |
| ServerSideCursorsTest | 14 | 1 | 0 | 15 | 93.3% |
| SimpleUpdateDeleteTest | 8 | 0 | 0 | 8 | 100.0% |
| StringTest | 10 | 0 | 0 | 10 | 100.0% |
| TableDDLTest | 3 | 0 | 7 | 10 | 100.0% |
| TempTableElementsTest | 0 | 0 | 2 | 2 | — |
| TextTest | 8 | 0 | 0 | 8 | 100.0% |
| TimeMicrosecondsTest | 5 | 0 | 1 | 6 | 100.0% |
| TimeTest | 5 | 0 | 1 | 6 | 100.0% |
| TrueDivTest | 9 | 0 | 0 | 9 | 100.0% |
| UnicodeTextTest | 6 | 0 | 0 | 6 | 100.0% |
| UnicodeVarcharTest | 6 | 0 | 0 | 6 | 100.0% |
| UuidTest | 7 | 0 | 0 | 7 | 100.0% |
| **total** | **713** | **22** | **680** | **1415** | **97.0%** |

## Failures (22)

- `test_suite.py::ComponentReflectionTestExtra_postgresql+psycopg_15_0::test_reflect_covering_index`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_in_unions_from_alias`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_selectable_in_unions`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_order_by_selectable_in_unions`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_select_from_plain_union`
- `test_suite.py::DateTimeMicrosecondsTest_postgresql+psycopg_15_0::test_round_trip`
- `test_suite.py::DateTimeMicrosecondsTest_postgresql+psycopg_15_0::test_round_trip_decorated`
- `test_suite.py::DeprecatedCompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_selectable_in_unions`
- `test_suite.py::DeprecatedCompoundSelectTest_postgresql+psycopg_15_0::test_order_by_selectable_in_unions`
- `test_suite.py::DistinctOnTest_postgresql+psycopg_15_0::test_distinct_on`
- `test_suite.py::ExistsTest_postgresql+psycopg_15_0::test_select_exists`
- `test_suite.py::ExistsTest_postgresql+psycopg_15_0::test_select_exists_false`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_limit_render_multiple_times`
- `test_suite.py::InsertBehaviorTest_postgresql+psycopg_15_0::test_insert_from_select_with_defaults`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_0-8.5514716-True-_exclusions_00]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_2-8.5514-True-_exclusions_02]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_4-8.5514716-True-_exclusions_04]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_5-value5-False-_exclusions_05]`
- `test_suite.py::RowFetchTest_postgresql+psycopg_15_0::test_row_w_scalar_select`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_0::test_insert_lastrowid`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_0::test_insert_roundtrip`
- `test_suite.py::ServerSideCursorsTest_postgresql+psycopg_15_0::test_aliases_and_ss`

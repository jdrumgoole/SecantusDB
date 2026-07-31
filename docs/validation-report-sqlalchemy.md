# SQLAlchemy dialect-compliance report

- SecantusDB (Python server) 0.6.0b6
- suite: sqlalchemy.testing.suite @ SQLAlchemy 2.0.51, postgresql+psycopg dialect
- generated: 2026-07-31 10:46 UTC

| suite class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| ArgSignatureTest | 22 | 0 | 0 | 22 | 100.0% |
| BinaryTest | 3 | 0 | 0 | 3 | 100.0% |
| BitwiseTest | 0 | 0 | 6 | 6 | — |
| BizarroCharacterTest | 2 | 30 | 32 | 64 | 6.2% |
| BooleanTest | 4 | 0 | 0 | 4 | 100.0% |
| CastTypeDecoratorTest | 1 | 0 | 0 | 1 | 100.0% |
| CollateTest | 0 | 0 | 1 | 1 | — |
| ComponentReflectionTest | 194 | 62 | 493 | 749 | 75.8% |
| ComponentReflectionTestExtra | 1 | 4 | 35 | 40 | 20.0% |
| CompositeKeyReflectionTest | 1 | 1 | 0 | 2 | 50.0% |
| CompoundSelectTest | 3 | 4 | 0 | 7 | 42.9% |
| DateTest | 4 | 1 | 1 | 6 | 80.0% |
| DateTimeCoercedToDateTimeTest | 4 | 1 | 1 | 6 | 80.0% |
| DateTimeMicrosecondsTest | 1 | 3 | 1 | 5 | 25.0% |
| DateTimeTest | 4 | 1 | 1 | 6 | 80.0% |
| DeprecatedCompoundSelectTest | 3 | 2 | 0 | 5 | 60.0% |
| DifficultParametersTest | 63 | 0 | 0 | 63 | 100.0% |
| DistinctOnTest | 0 | 1 | 0 | 1 | 0.0% |
| EnumTest | 7 | 0 | 0 | 7 | 100.0% |
| EscapingTest | 1 | 0 | 0 | 1 | 100.0% |
| ExceptionTest | 2 | 0 | 0 | 2 | 100.0% |
| ExistsTest | 0 | 2 | 0 | 2 | 0.0% |
| ExpandingBoundInTest | 18 | 0 | 11 | 29 | 100.0% |
| FetchLimitOffsetTest | 10 | 6 | 13 | 29 | 62.5% |
| FutureTableDDLTest | 3 | 0 | 7 | 10 | 100.0% |
| HasIndexTest | 2 | 0 | 2 | 4 | 100.0% |
| HasSequenceTest | 0 | 11 | 0 | 11 | 0.0% |
| HasSequenceTestEmpty | 1 | 0 | 0 | 1 | 100.0% |
| HasTableTest | 3 | 0 | 5 | 8 | 100.0% |
| IdentityAutoincrementTest | 1 | 0 | 0 | 1 | 100.0% |
| InsertBehaviorTest | 10 | 2 | 0 | 12 | 83.3% |
| IntegerTest | 23 | 0 | 0 | 23 | 100.0% |
| IsOrIsNotDistinctFromTest | 0 | 5 | 0 | 5 | 0.0% |
| JoinTest | 5 | 0 | 0 | 5 | 100.0% |
| LastrowidTest | 2 | 0 | 1 | 3 | 100.0% |
| LikeFunctionsTest | 2 | 12 | 9 | 23 | 14.3% |
| LongNameBlowoutTest | 4 | 0 | 1 | 5 | 100.0% |
| NumericTest | 14 | 4 | 3 | 21 | 77.8% |
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
| TimeMicrosecondsTest | 4 | 1 | 1 | 6 | 80.0% |
| TimeTest | 4 | 1 | 1 | 6 | 80.0% |
| TrueDivTest | 5 | 4 | 0 | 9 | 55.6% |
| UnicodeTextTest | 6 | 0 | 0 | 6 | 100.0% |
| UnicodeVarcharTest | 6 | 0 | 0 | 6 | 100.0% |
| UuidTest | 7 | 0 | 0 | 7 | 100.0% |
| **total** | **572** | **166** | **677** | **1415** | **77.5%** |

## Failures (166)

- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-(3)-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-(3)-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-[brack]-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-[brack]-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-col%p-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-col%p-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-plainname-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[(2)-plainname-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-(3)-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-(3)-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-[brack]-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-[brack]-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-col%p-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-col%p-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-plainname-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[[brackets]-plainname-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-(3)-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-(3)-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-[brack]-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-[brack]-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-col%p-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-col%p-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-plainname-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[per % cent-plainname-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-(3)-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-(3)-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-[brack]-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-[brack]-use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-col%p-not_use_composite]`
- `test_suite.py::BizarroCharacterTest_postgresql+psycopg_15_0::test_fk_ref[plain-col%p-use_composite]`
- `test_suite.py::ComponentReflectionTestExtra_postgresql+psycopg_15_0::test_numeric_reflection`
- `test_suite.py::ComponentReflectionTestExtra_postgresql+psycopg_15_0::test_string_length_reflection[CHAR-_exclusions_02]`
- `test_suite.py::ComponentReflectionTestExtra_postgresql+psycopg_15_0::test_string_length_reflection[String-_exclusions_00]`
- `test_suite.py::ComponentReflectionTestExtra_postgresql+psycopg_15_0::test_string_length_reflection[VARCHAR-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_autoincrement_col`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_columns[True-False-_exclusions_02]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.ANY_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.ANY_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[False-ObjectKind.VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.ANY_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.ANY_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_columns[True-ObjectKind.VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_foreign_keys[False-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_indexes[False-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_pk_constraint[False-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.ANY-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.ANY-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_multi_unique_constraints[False-ObjectKind.TABLE|VIEW-ObjectScope.DEFAULT-None-_exclusions_00]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_table_names[False-_exclusions_01-None-_exclusions_10]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_table_names[False-_exclusions_01-foreign_key-_exclusions_11]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_get_temp_table_indexes`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_0::test_reflect_table_temp_table`
- `test_suite.py::CompositeKeyReflectionTest_postgresql+psycopg_15_0::test_pk_column_order`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_in_unions_from_alias`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_selectable_in_unions`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_order_by_selectable_in_unions`
- `test_suite.py::CompoundSelectTest_postgresql+psycopg_15_0::test_select_from_plain_union`
- `test_suite.py::DateTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::DateTimeCoercedToDateTimeTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::DateTimeMicrosecondsTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::DateTimeMicrosecondsTest_postgresql+psycopg_15_0::test_round_trip`
- `test_suite.py::DateTimeMicrosecondsTest_postgresql+psycopg_15_0::test_round_trip_decorated`
- `test_suite.py::DateTimeTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::DeprecatedCompoundSelectTest_postgresql+psycopg_15_0::test_limit_offset_selectable_in_unions`
- `test_suite.py::DeprecatedCompoundSelectTest_postgresql+psycopg_15_0::test_order_by_selectable_in_unions`
- `test_suite.py::DistinctOnTest_postgresql+psycopg_15_0::test_distinct_on`
- `test_suite.py::ExistsTest_postgresql+psycopg_15_0::test_select_exists`
- `test_suite.py::ExistsTest_postgresql+psycopg_15_0::test_select_exists_false`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_expr_limit`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_expr_limit_offset`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_expr_limit_simple_offset`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_expr_offset`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_limit_render_multiple_times`
- `test_suite.py::FetchLimitOffsetTest_postgresql+psycopg_15_0::test_simple_limit_expr_offset`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_get_sequence_names`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_get_sequence_names_no_sequence_schema`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_get_sequence_names_sequences_schema`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_cache`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_default_not_in_remote`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_neg`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_other_object`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_remote_not_in_default`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_schema`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_0::test_has_sequence_schemas_neg`
- `test_suite.py::InsertBehaviorTest_postgresql+psycopg_15_0::test_empty_insert`
- `test_suite.py::InsertBehaviorTest_postgresql+psycopg_15_0::test_insert_from_select_with_defaults`
- `test_suite.py::IsOrIsNotDistinctFromTest_postgresql+psycopg_15_0::test_is_or_is_not_distinct_from[both_int_different]`
- `test_suite.py::IsOrIsNotDistinctFromTest_postgresql+psycopg_15_0::test_is_or_is_not_distinct_from[both_int_same]`
- `test_suite.py::IsOrIsNotDistinctFromTest_postgresql+psycopg_15_0::test_is_or_is_not_distinct_from[both_null]`
- `test_suite.py::IsOrIsNotDistinctFromTest_postgresql+psycopg_15_0::test_is_or_is_not_distinct_from[one_null_first]`
- `test_suite.py::IsOrIsNotDistinctFromTest_postgresql+psycopg_15_0::test_is_or_is_not_distinct_from[one_null_second]`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_contains_autoescape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_contains_autoescape_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_contains_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_contains_unescaped`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_endswith_autoescape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_endswith_autoescape_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_endswith_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_endswith_unescaped`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_startswith_autoescape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_startswith_autoescape_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_startswith_escape`
- `test_suite.py::LikeFunctionsTest_postgresql+psycopg_15_0::test_startswith_unescaped`
- `test_suite.py::NumericTest_postgresql+psycopg_15_0::test_float_as_decimal`
- `test_suite.py::NumericTest_postgresql+psycopg_15_0::test_float_custom_scale`
- `test_suite.py::NumericTest_postgresql+psycopg_15_0::test_numeric_as_float`
- `test_suite.py::NumericTest_postgresql+psycopg_15_0::test_numeric_null_as_float`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_0-8.5514716-True-_exclusions_00]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_2-8.5514-True-_exclusions_02]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_4-8.5514716-True-_exclusions_04]`
- `test_suite.py::ReturningTest_postgresql+psycopg_15_0::test_insert_w_floats[multiple_rows-sort_by_parameter_order-type_5-value5-False-_exclusions_05]`
- `test_suite.py::RowFetchTest_postgresql+psycopg_15_0::test_row_w_scalar_select`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_0::test_insert_lastrowid`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_0::test_insert_roundtrip`
- `test_suite.py::ServerSideCursorsTest_postgresql+psycopg_15_0::test_aliases_and_ss`
- `test_suite.py::TimeMicrosecondsTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::TimeTest_postgresql+psycopg_15_0::test_null_bound_comparison`
- `test_suite.py::TrueDivTest_postgresql+psycopg_15_0::test_truediv_integer[-15-10--1.5]`
- `test_suite.py::TrueDivTest_postgresql+psycopg_15_0::test_truediv_integer[15-10-1.5]`
- `test_suite.py::TrueDivTest_postgresql+psycopg_15_0::test_truediv_integer_bound`
- `test_suite.py::TrueDivTest_postgresql+psycopg_15_0::test_truediv_numeric[5.52-2.4-2.3]`

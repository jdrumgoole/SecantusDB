# SQLAlchemy dialect-compliance report

- SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17
- suite: sqlalchemy.testing.suite @ SQLAlchemy 2.0.51, postgresql+psycopg dialect
- generated: 2026-11-09 06:57 UTC

| suite class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| ArgSignatureTest | 22 | 0 | 0 | 22 | 100.0% |
| BinaryTest | 3 | 0 | 0 | 3 | 100.0% |
| BitwiseTest | 0 | 0 | 6 | 6 | — |
| BizarroCharacterTest | 32 | 0 | 32 | 64 | 100.0% |
| BooleanTest | 4 | 0 | 0 | 4 | 100.0% |
| CastTypeDecoratorTest | 1 | 0 | 0 | 1 | 100.0% |
| CollateTest | 0 | 0 | 1 | 1 | — |
| ComponentReflectionTest | 467 | 18 | 264 | 749 | 96.2% |
| ComponentReflectionTestExtra | 6 | 0 | 34 | 40 | 100.0% |
| CompositeKeyReflectionTest | 2 | 0 | 0 | 2 | 100.0% |
| CompoundSelectTest | 7 | 0 | 0 | 7 | 100.0% |
| DateTest | 5 | 0 | 1 | 6 | 100.0% |
| DateTimeCoercedToDateTimeTest | 5 | 0 | 1 | 6 | 100.0% |
| DateTimeTest | 5 | 0 | 1 | 6 | 100.0% |
| DeprecatedCompoundSelectTest | 5 | 0 | 0 | 5 | 100.0% |
| DifficultParametersTest | 63 | 0 | 0 | 63 | 100.0% |
| DistinctOnTest | 1 | 0 | 0 | 1 | 100.0% |
| EnumTest | 7 | 0 | 0 | 7 | 100.0% |
| EscapingTest | 1 | 0 | 0 | 1 | 100.0% |
| ExceptionTest | 2 | 0 | 0 | 2 | 100.0% |
| ExistsTest | 2 | 0 | 0 | 2 | 100.0% |
| ExpandingBoundInTest | 18 | 0 | 11 | 29 | 100.0% |
| FetchLimitOffsetTest | 16 | 0 | 13 | 29 | 100.0% |
| FutureTableDDLTest | 4 | 0 | 6 | 10 | 100.0% |
| HasIndexTest | 0 | 4 | 0 | 4 | 0.0% |
| HasSequenceTest | 10 | 1 | 0 | 11 | 90.9% |
| HasSequenceTestEmpty | 1 | 0 | 0 | 1 | 100.0% |
| HasTableTest | 5 | 2 | 1 | 8 | 71.4% |
| IdentityAutoincrementTest | 1 | 0 | 0 | 1 | 100.0% |
| InsertBehaviorTest | 12 | 0 | 0 | 12 | 100.0% |
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
| ReturningTest | 23 | 0 | 34 | 57 | 100.0% |
| RowCountTest | 20 | 0 | 0 | 20 | 100.0% |
| RowFetchTest | 6 | 0 | 0 | 6 | 100.0% |
| SameNamedSchemaTableTest | 3 | 0 | 0 | 3 | 100.0% |
| SequenceCompilerTest | 1 | 0 | 0 | 1 | 100.0% |
| SequenceTest | 1 | 7 | 0 | 8 | 12.5% |
| ServerSideCursorsTest | 15 | 0 | 0 | 15 | 100.0% |
| SimpleUpdateDeleteTest | 8 | 0 | 0 | 8 | 100.0% |
| StringTest | 10 | 0 | 0 | 10 | 100.0% |
| TableDDLTest | 3 | 1 | 6 | 10 | 75.0% |
| TempTableElementsTest | 0 | 0 | 2 | 2 | — |
| TextTest | 8 | 0 | 0 | 8 | 100.0% |
| TimeMicrosecondsTest | 5 | 0 | 1 | 6 | 100.0% |
| TimeTest | 5 | 0 | 1 | 6 | 100.0% |
| TrueDivTest | 9 | 0 | 0 | 9 | 100.0% |
| UnicodeTextTest | 6 | 0 | 0 | 6 | 100.0% |
| UnicodeVarcharTest | 6 | 0 | 0 | 6 | 100.0% |
| UuidTest | 7 | 0 | 0 | 7 | 100.0% |
| **total** | **945** | **33** | **435** | **1413** | **96.6%** |

## Failures (33)

- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.ANY-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.ANY_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.TABLE-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_indexes[False-ObjectKind.VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.ANY-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.ANY_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.TABLE-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_pk_constraint[False-ObjectKind.VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.ANY-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.ANY_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.TABLE-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.TABLE|MATERIALIZED_VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.TABLE|VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::ComponentReflectionTest_postgresql+psycopg_15_19::test_get_multi_unique_constraints[False-ObjectKind.VIEW-ObjectScope.ANY-<lambda>-_exclusions_01]`
- `test_suite.py::HasIndexTest_postgresql+psycopg_15_19::test_has_index[dialect]`
- `test_suite.py::HasIndexTest_postgresql+psycopg_15_19::test_has_index[inspector]`
- `test_suite.py::HasIndexTest_postgresql+psycopg_15_19::test_has_index_schema[dialect]`
- `test_suite.py::HasIndexTest_postgresql+psycopg_15_19::test_has_index_schema[inspector]`
- `test_suite.py::HasSequenceTest_postgresql+psycopg_15_19::test_get_sequence_names_sequences_schema`
- `test_suite.py::HasTableTest_postgresql+psycopg_15_19::test_has_table_schema`
- `test_suite.py::HasTableTest_postgresql+psycopg_15_19::test_has_table_view_schema`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_insert_roundtrip`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_insert_roundtrip_no_implicit_returning`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_insert_roundtrip_translate[False]`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_insert_roundtrip_translate[True]`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_nextval_direct`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_nextval_direct_schema_translate`
- `test_suite.py::SequenceTest_postgresql+psycopg_15_19::test_optional_seq`
- `test_suite.py::TableDDLTest_postgresql+psycopg_15_19::test_create_table_schema`

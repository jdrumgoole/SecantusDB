# pgjdbc conformance report

- SecantusDB (Python server) 0.6.0b11
- suite: vendor/pgjdbc @ 3297557c6a80 (Gradle `:postgresql:test`, unmodified; 60s JUnit default timeout injected)
- generated: 2026-08-16 10:14 UTC

| test class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| jdbc2.ArrayTest | 42 | 4 | 0 | 46 | 91.3% |
| jdbc2.AutoRollbackTest | 1056 | 0 | 0 | 1056 | 100.0% |
| jdbc2.AutoSaveTransactionSettingsTest | 4 | 2 | 0 | 6 | 66.7% |
| jdbc2.BatchDeadlockTest | 2 | 6 | 8 | 16 | 25.0% |
| jdbc2.BatchExecuteTest | 140 | 0 | 0 | 140 | 100.0% |
| jdbc2.BatchFailureTest | 184 | 0 | 0 | 184 | 100.0% |
| jdbc2.BatchedInsertReWriteEnabledTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.BlobTest | 28 | 0 | 0 | 28 | 100.0% |
| jdbc2.BlobTransactionTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.CallableStmtTest | 14 | 0 | 0 | 14 | 100.0% |
| jdbc2.CleanupSavepointsWithFastpathTest | 10 | 0 | 0 | 10 | 100.0% |
| jdbc2.ClientEncodingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.ColumnSanitiserDisabledTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ColumnSanitiserEnabledTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ConcurrentStatementFetchTest | 12 | 0 | 0 | 12 | 100.0% |
| jdbc2.ConnectExecutorTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.ConnectTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.ConnectionSetupFailureTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.ConnectionTest | 15 | 0 | 0 | 15 | 100.0% |
| jdbc2.CopyLargeFileTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.CopyTest | 24 | 0 | 0 | 24 | 100.0% |
| jdbc2.CursorFetchSqlTransactionTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.CursorFetchTest | 32 | 0 | 0 | 32 | 100.0% |
| jdbc2.CustomTypeWithBinaryTransferTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.DatabaseEncodingTest | 2 | 1 | 0 | 3 | 66.7% |
| jdbc2.DatabaseMetaDataCacheTest | 2 | 1 | 0 | 3 | 66.7% |
| jdbc2.DatabaseMetaDataPropertiesTest | 12 | 1 | 0 | 13 | 92.3% |
| jdbc2.DatabaseMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.DatabaseMetaDataTransactionIsolationTest | 14 | 0 | 0 | 14 | 100.0% |
| jdbc2.DateStyleTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.DateTest | 178 | 0 | 14 | 192 | 100.0% |
| jdbc2.DriverTest | 16 | 0 | 1 | 17 | 100.0% |
| jdbc2.EncodingTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.EnumTest | 2 | 2 | 0 | 4 | 50.0% |
| jdbc2.GeometricTest | 12 | 2 | 0 | 14 | 85.7% |
| jdbc2.GetXXXTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.IntervalTest | 26 | 0 | 0 | 26 | 100.0% |
| jdbc2.JBuilderTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.LoginTimeoutInterruptTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.LoginTimeoutTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.MiscTest | 4 | 0 | 1 | 5 | 100.0% |
| jdbc2.NumericTransfer2Test | 152 | 4 | 0 | 156 | 97.4% |
| jdbc2.NumericTransferTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.OuterJoinSyntaxTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.PGObjectGetTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.PGObjectSetTest | 40 | 0 | 0 | 40 | 100.0% |
| jdbc2.PGPropertyTest | 11 | 0 | 0 | 11 | 100.0% |
| jdbc2.PGTimeTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.PGTimestampTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ParameterStatusTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.PreparedStatementTest | 100 | 6 | 4 | 110 | 94.3% |
| jdbc2.QuotationTest | 2912 | 0 | 0 | 2912 | 100.0% |
| jdbc2.RefCursorFetchTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.RefCursorTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ReplaceProcessingTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ResultSetMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ResultSetRefreshTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ResultSetTest | 82 | 2 | 0 | 84 | 97.6% |
| jdbc2.SearchPathLookupTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.ServerCursorTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ServerErrorTest | 7 | 0 | 0 | 7 | 100.0% |
| jdbc2.ServerPreparedStmtTest | 13 | 0 | 0 | 13 | 100.0% |
| jdbc2.SharedTimerRefCountTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.SocketTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.StatementTest | 37 | 5 | 0 | 42 | 88.1% |
| jdbc2.StringTypeUnspecifiedArrayTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.TimeTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.TimestampTest | 6 | 8 | 0 | 14 | 42.9% |
| jdbc2.TimezoneCachingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.TimezoneTest | 16 | 0 | 0 | 16 | 100.0% |
| jdbc2.TransactionRoundtripTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.TransactionStateTest | 19 | 0 | 0 | 19 | 100.0% |
| jdbc2.TypeCacheDLLStressTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.UpdateableResultTest | 32 | 4 | 0 | 36 | 88.9% |
| jdbc2.UpsertTest | 32 | 0 | 0 | 32 | 100.0% |
| **total** | **5512** | **58** | **28** | **5598** | **99.0%** |

## Failures (58)

- `jdbc2.ArrayTest :: testNonStandardBounds()`
- `jdbc2.ArrayTest :: testNonStandardBounds()`
- `jdbc2.ArrayTest :: testUnknownArrayType()`
- `jdbc2.ArrayTest :: testUnknownArrayType()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setLocalTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setLocalTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.CursorFetchSqlTransactionTest :: [1] startTransaction=BEGIN`
- `jdbc2.CursorFetchSqlTransactionTest :: [2] startTransaction=START TRANSACTION`
- `jdbc2.CursorFetchSqlTransactionTest :: [3] startTransaction=START TRANSACTION READ ONLY`
- `jdbc2.CustomTypeWithBinaryTransferTest :: testCustomBinaryTypes()`
- `jdbc2.CustomTypeWithBinaryTransferTest :: testCustomBinaryTypes()`
- `jdbc2.DatabaseEncodingTest :: encoding()`
- `jdbc2.DatabaseMetaDataCacheTest :: getSQLTypeQueryCache()`
- `jdbc2.DatabaseMetaDataPropertiesTest :: values()`
- `jdbc2.DatabaseMetaDataTest :: initializationError`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.LoginTimeoutInterruptTest :: loginTimeoutInterruptsAuthPluginSleep()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.PreparedStatementTest :: testDoubleQuestionMark()`
- `jdbc2.PreparedStatementTest :: testDoubleQuestionMark()`
- `jdbc2.PreparedStatementTest :: testNumeric()`
- `jdbc2.PreparedStatementTest :: testNumeric()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.RefCursorFetchTest :: initializationError`
- `jdbc2.RefCursorTest :: initializationError`
- `jdbc2.ResultSetMetaDataTest :: initializationError`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.StatementTest :: closeInProgressStatement()`
- `jdbc2.StatementTest :: closeInProgressStatementProtocol32()`
- `jdbc2.StatementTest :: concurrentWarningReadAndClear()`
- `jdbc2.StatementTest :: parsingSemiColons()`
- `jdbc2.StatementTest :: warningsAreAvailableAsap()`
- `jdbc2.TimestampTest :: testGetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWTZ()`
- `jdbc2.UpdateableResultTest :: testOidUpdatable()`
- `jdbc2.UpdateableResultTest :: testReturnSerial()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithLocalDateTime()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithOffsetDateTime()`

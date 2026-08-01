# pgjdbc conformance report

- SecantusDB (Python server) 0.6.0b7
- suite: vendor/pgjdbc @ 3297557c6a80 (Gradle `:postgresql:test`, unmodified; 60s JUnit default timeout injected)
- generated: 2026-08-01 09:44 UTC

| test class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| jdbc2.ArrayTest | 42 | 4 | 0 | 46 | 91.3% |
| jdbc2.AutoRollbackTest | 1000 | 56 | 0 | 1056 | 94.7% |
| jdbc2.AutoSaveTransactionSettingsTest | 0 | 6 | 0 | 6 | 0.0% |
| jdbc2.BatchDeadlockTest | 2 | 6 | 8 | 16 | 25.0% |
| jdbc2.BatchExecuteTest | 128 | 12 | 0 | 140 | 91.4% |
| jdbc2.BatchFailureTest | 96 | 88 | 0 | 184 | 52.2% |
| jdbc2.BatchedInsertReWriteEnabledTest | 44 | 16 | 0 | 60 | 73.3% |
| jdbc2.BlobTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.BlobTransactionTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.CallableStmtTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.CleanupSavepointsWithFastpathTest | 0 | 10 | 0 | 10 | 0.0% |
| jdbc2.ClientEncodingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.ColumnSanitiserDisabledTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ColumnSanitiserEnabledTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ConcurrentStatementFetchTest | 12 | 0 | 0 | 12 | 100.0% |
| jdbc2.ConnectExecutorTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.ConnectTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.ConnectionSetupFailureTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.ConnectionTest | 11 | 4 | 0 | 15 | 73.3% |
| jdbc2.CopyLargeFileTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.CopyTest | 23 | 1 | 0 | 24 | 95.8% |
| jdbc2.CursorFetchSqlTransactionTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.CursorFetchTest | 32 | 0 | 0 | 32 | 100.0% |
| jdbc2.CustomTypeWithBinaryTransferTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.DatabaseEncodingTest | 2 | 1 | 0 | 3 | 66.7% |
| jdbc2.DatabaseMetaDataCacheTest | 1 | 2 | 0 | 3 | 33.3% |
| jdbc2.DatabaseMetaDataPropertiesTest | 12 | 1 | 0 | 13 | 92.3% |
| jdbc2.DatabaseMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.DatabaseMetaDataTransactionIsolationTest | 0 | 14 | 0 | 14 | 0.0% |
| jdbc2.DateStyleTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.DateTest | 74 | 104 | 14 | 192 | 41.6% |
| jdbc2.DriverTest | 16 | 0 | 1 | 17 | 100.0% |
| jdbc2.EncodingTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.EnumTest | 0 | 4 | 0 | 4 | 0.0% |
| jdbc2.GeometricTest | 0 | 14 | 0 | 14 | 0.0% |
| jdbc2.GetXXXTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.IntervalTest | 22 | 4 | 0 | 26 | 84.6% |
| jdbc2.JBuilderTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.LoginTimeoutInterruptTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.LoginTimeoutTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.MiscTest | 4 | 0 | 1 | 5 | 100.0% |
| jdbc2.NumericTransfer2Test | 130 | 26 | 0 | 156 | 83.3% |
| jdbc2.NumericTransferTest | 2 | 2 | 0 | 4 | 50.0% |
| jdbc2.OuterJoinSyntaxTest | 0 | 6 | 0 | 6 | 0.0% |
| jdbc2.PGObjectGetTest | 32 | 28 | 0 | 60 | 53.3% |
| jdbc2.PGObjectSetTest | 14 | 26 | 0 | 40 | 35.0% |
| jdbc2.PGPropertyTest | 11 | 0 | 0 | 11 | 100.0% |
| jdbc2.PGTimeTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.PGTimestampTest | 1 | 1 | 0 | 2 | 50.0% |
| jdbc2.ParameterStatusTest | 6 | 3 | 0 | 9 | 66.7% |
| jdbc2.PreparedStatementTest | 92 | 14 | 4 | 110 | 86.8% |
| jdbc2.QuotationTest | 2912 | 0 | 0 | 2912 | 100.0% |
| jdbc2.RefCursorFetchTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.RefCursorTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ReplaceProcessingTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ResultSetMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ResultSetRefreshTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.ResultSetTest | 76 | 8 | 0 | 84 | 90.5% |
| jdbc2.SearchPathLookupTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.ServerCursorTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ServerErrorTest | 0 | 7 | 0 | 7 | 0.0% |
| jdbc2.ServerPreparedStmtTest | 13 | 0 | 0 | 13 | 100.0% |
| jdbc2.SharedTimerRefCountTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.SocketTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.StatementTest | 27 | 15 | 0 | 42 | 64.3% |
| jdbc2.StringTypeUnspecifiedArrayTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.TimeTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.TimestampTest | 6 | 8 | 0 | 14 | 42.9% |
| jdbc2.TimezoneCachingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.TimezoneTest | 1 | 15 | 0 | 16 | 6.2% |
| jdbc2.TransactionRoundtripTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.TransactionStateTest | 18 | 1 | 0 | 19 | 94.7% |
| jdbc2.TypeCacheDLLStressTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.UpdateableResultTest | 3 | 33 | 0 | 36 | 8.3% |
| jdbc2.UpsertTest | 32 | 0 | 0 | 32 | 100.0% |
| **total** | **4962** | **568** | **28** | **5558** | **89.7%** |

## Failures (568)

- `jdbc2.ArrayTest :: testNonStandardBounds()`
- `jdbc2.ArrayTest :: testNonStandardBounds()`
- `jdbc2.ArrayTest :: testUnknownArrayType()`
- `jdbc2.ArrayTest :: testUnknownArrayType()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoRollbackTest :: run()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setLocalTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setLocalTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setSessionTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setSessionTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.AutoSaveTransactionSettingsTest :: setTransactionIsolationLevelAsFirstStatement()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchDeadlockTest :: largePreparedBatchWithGeneratedKeysDoesNotDeadlock()`
- `jdbc2.BatchExecuteTest :: testPreparedStatement()`
- `jdbc2.BatchExecuteTest :: testPreparedStatement()`
- `jdbc2.BatchExecuteTest :: testPreparedStatement()`
- `jdbc2.BatchExecuteTest :: testPreparedStatement()`
- `jdbc2.BatchExecuteTest :: testSelectInBatchThrowsAutoCommit()`
- `jdbc2.BatchExecuteTest :: testSelectInBatchThrowsAutoCommit()`
- `jdbc2.BatchExecuteTest :: testSelectInBatchThrowsAutoCommit()`
- `jdbc2.BatchExecuteTest :: testSelectInBatchThrowsAutoCommit()`
- `jdbc2.BatchExecuteTest :: testSmallBatchUpdateFailureSimple()`
- `jdbc2.BatchExecuteTest :: testSmallBatchUpdateFailureSimple()`
- `jdbc2.BatchExecuteTest :: testSmallBatchUpdateFailureSimple()`
- `jdbc2.BatchExecuteTest :: testSmallBatchUpdateFailureSimple()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchFailureTest :: run()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32767Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32767Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32767Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32767Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32768Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32768Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32768Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test32768Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test65535Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test65535Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test65535Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: test65535Binds()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: testBatchWithReWrittenRepeatedInsertStatementOptimizationEnabled()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: testBatchWithReWrittenRepeatedInsertStatementOptimizationEnabled()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: testBatchWithReWrittenRepeatedInsertStatementOptimizationEnabled()`
- `jdbc2.BatchedInsertReWriteEnabledTest :: testBatchWithReWrittenRepeatedInsertStatementOptimizationEnabled()`
- `jdbc2.BlobTest :: initializationError`
- `jdbc2.BlobTransactionTest :: initializationError`
- `jdbc2.CallableStmtTest :: initializationError`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testInterleavedQueriesAndLargeObjects()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testInterleavedQueriesAndLargeObjects()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testLargeObjectWithCleanupSavepoints()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testLargeObjectWithCleanupSavepoints()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testLargeObjectWithoutCleanupSavepoints()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testLargeObjectWithoutCleanupSavepoints()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testMultipleQueriesThenLargeObject()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testMultipleQueriesThenLargeObject()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testPreparedStatementThenLargeObject()`
- `jdbc2.CleanupSavepointsWithFastpathTest :: testPreparedStatementThenLargeObject()`
- `jdbc2.ConnectionTest :: pGStreamSettings()`
- `jdbc2.ConnectionTest :: readOnly_always()`
- `jdbc2.ConnectionTest :: readOnly_transaction()`
- `jdbc2.ConnectionTest :: transactionIsolation()`
- `jdbc2.CopyLargeFileTest :: feedTableSeveralTimesTest()`
- `jdbc2.CopyTest :: copyMultiApi()`
- `jdbc2.CursorFetchSqlTransactionTest :: [1] startTransaction=BEGIN`
- `jdbc2.CursorFetchSqlTransactionTest :: [2] startTransaction=START TRANSACTION`
- `jdbc2.CursorFetchSqlTransactionTest :: [3] startTransaction=START TRANSACTION READ ONLY`
- `jdbc2.CustomTypeWithBinaryTransferTest :: testCustomBinaryTypes()`
- `jdbc2.CustomTypeWithBinaryTransferTest :: testCustomBinaryTypes()`
- `jdbc2.DatabaseEncodingTest :: encoding()`
- `jdbc2.DatabaseMetaDataCacheTest :: getSQLTypeQueryCache()`
- `jdbc2.DatabaseMetaDataCacheTest :: getTypeInfoUsesCache()`
- `jdbc2.DatabaseMetaDataPropertiesTest :: values()`
- `jdbc2.DatabaseMetaDataTest :: initializationError`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [1] isolationLevel=8`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [1] isolationLevel=read committed`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [1] isolationLevel=read committed`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [2] isolationLevel=4`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [2] isolationLevel=read uncommitted`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [2] isolationLevel=read uncommitted`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [3] isolationLevel=2`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [3] isolationLevel=repeatable read`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [3] isolationLevel=repeatable read`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [4] isolationLevel=1`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [4] isolationLevel=serializable`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [4] isolationLevel=serializable`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: connectionTransactionIsolation()`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: metadataDefaultTransactionIsolation()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.DateTest :: testSetDate()`
- `jdbc2.EnumTest :: enumArray()`
- `jdbc2.EnumTest :: enumArray()`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.GeometricTest :: testPGbox()`
- `jdbc2.GeometricTest :: testPGbox()`
- `jdbc2.GeometricTest :: testPGcircle()`
- `jdbc2.GeometricTest :: testPGcircle()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.GeometricTest :: testPGlseg()`
- `jdbc2.GeometricTest :: testPGlseg()`
- `jdbc2.GeometricTest :: testPGpath()`
- `jdbc2.GeometricTest :: testPGpath()`
- `jdbc2.GeometricTest :: testPGpoint()`
- `jdbc2.GeometricTest :: testPGpoint()`
- `jdbc2.GeometricTest :: testPGpolygon()`
- `jdbc2.GeometricTest :: testPGpolygon()`
- `jdbc2.GetXXXTest :: getObject()`
- `jdbc2.GetXXXTest :: getUDT()`
- `jdbc2.IntervalTest :: daysHours()`
- `jdbc2.IntervalTest :: onlineTests()`
- `jdbc2.IntervalTest :: smallValue()`
- `jdbc2.IntervalTest :: stringToIntervalCoercion()`
- `jdbc2.LoginTimeoutInterruptTest :: loginTimeoutInterruptsAuthPluginSleep()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransferTest :: receive100000()`
- `jdbc2.NumericTransferTest :: receive100000()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj2()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj3()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithoutOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithSingleJoinAndWithOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithSingleJoinAndWithoutOj()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobject()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectGetTest :: getAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobject()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGObjectSetTest :: setNullAsPGobjectSubtype()`
- `jdbc2.PGTimeTest :: testTimeInsertAndSelect()`
- `jdbc2.PGTimeTest :: testTimeWithInterval()`
- `jdbc2.PGTimestampTest :: timestampWithInterval()`
- `jdbc2.ParameterStatusTest :: expectedInitialParameters()`
- `jdbc2.ParameterStatusTest :: transactionalParametersCommit()`
- `jdbc2.ParameterStatusTest :: transactionalParametersRollback()`
- `jdbc2.PreparedStatementTest :: testComments()`
- `jdbc2.PreparedStatementTest :: testComments()`
- `jdbc2.PreparedStatementTest :: testDollarQuotes()`
- `jdbc2.PreparedStatementTest :: testDollarQuotes()`
- `jdbc2.PreparedStatementTest :: testDoubleQuestionMark()`
- `jdbc2.PreparedStatementTest :: testDoubleQuestionMark()`
- `jdbc2.PreparedStatementTest :: testNumeric()`
- `jdbc2.PreparedStatementTest :: testNumeric()`
- `jdbc2.PreparedStatementTest :: testSetIntFloat()`
- `jdbc2.PreparedStatementTest :: testSetIntFloat()`
- `jdbc2.PreparedStatementTest :: testSingleQuotes()`
- `jdbc2.PreparedStatementTest :: testSingleQuotes()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.RefCursorFetchTest :: initializationError`
- `jdbc2.RefCursorTest :: initializationError`
- `jdbc2.ResultSetMetaDataTest :: initializationError`
- `jdbc2.ResultSetRefreshTest :: testWithDataColumnThatRequiresEscaping()`
- `jdbc2.ResultSetRefreshTest :: testWithKeyColumnThatRequiresEscaping()`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.ResultSetTest :: testTimestamp()`
- `jdbc2.ResultSetTest :: testTimestamp()`
- `jdbc2.ResultSetTest :: testUpdateWithPGobject()`
- `jdbc2.ResultSetTest :: testUpdateWithPGobject()`
- `jdbc2.ResultSetTest :: testgetBadBoolean()`
- `jdbc2.ResultSetTest :: testgetBadBoolean()`
- `jdbc2.SearchPathLookupTest :: searchPathBackwardsCompatibleLookup()`
- `jdbc2.SearchPathLookupTest :: searchPathHiddenLookup()`
- `jdbc2.SearchPathLookupTest :: searchPathNormalLookup()`
- `jdbc2.ServerErrorTest :: testCheckConstraint()`
- `jdbc2.ServerErrorTest :: testColumn()`
- `jdbc2.ServerErrorTest :: testDatatype()`
- `jdbc2.ServerErrorTest :: testExclusionConstraint()`
- `jdbc2.ServerErrorTest :: testForeignKeyConstraint()`
- `jdbc2.ServerErrorTest :: testNotNullConstraint()`
- `jdbc2.ServerErrorTest :: testPrimaryKey()`
- `jdbc2.SharedTimerRefCountTest :: multipleCancels()`
- `jdbc2.StatementTest :: closeInProgressStatement()`
- `jdbc2.StatementTest :: closeInProgressStatementProtocol32()`
- `jdbc2.StatementTest :: concurrentWarningReadAndClear()`
- `jdbc2.StatementTest :: dateFuncWithParam()`
- `jdbc2.StatementTest :: dateFunctions()`
- `jdbc2.StatementTest :: javaScriptFunction()`
- `jdbc2.StatementTest :: numericFunctions()`
- `jdbc2.StatementTest :: parsingDollarQuotes()`
- `jdbc2.StatementTest :: parsingSemiColons()`
- `jdbc2.StatementTest :: setQueryTimeout()`
- `jdbc2.StatementTest :: setQueryTimeoutOnPrepared()`
- `jdbc2.StatementTest :: setQueryTimeoutWithSleep()`
- `jdbc2.StatementTest :: stringFunctions()`
- `jdbc2.StatementTest :: updateCount()`
- `jdbc2.StatementTest :: warningsAreAvailableAsap()`
- `jdbc2.TimeTest :: getTime()`
- `jdbc2.TimeTest :: getTimeZone()`
- `jdbc2.TimeTest :: setTime()`
- `jdbc2.TimestampTest :: testGetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWTZ()`
- `jdbc2.TimestampTest :: testGetTimestampWTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWOTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWTZ()`
- `jdbc2.TimestampTest :: testSetTimestampWTZ()`
- `jdbc2.TimezoneTest :: getDate()`
- `jdbc2.TimezoneTest :: getTime()`
- `jdbc2.TimezoneTest :: getTimestamp()`
- `jdbc2.TimezoneTest :: halfHourTimezone()`
- `jdbc2.TimezoneTest :: localTimestampsInAfricaCasablanca()`
- `jdbc2.TimezoneTest :: localTimestampsInAmericaAdak()`
- `jdbc2.TimezoneTest :: localTimestampsInAtlanticAzores()`
- `jdbc2.TimezoneTest :: localTimestampsInEuropeMoscow()`
- `jdbc2.TimezoneTest :: localTimestampsInNonDSTZones()`
- `jdbc2.TimezoneTest :: localTimestampsInPacificApia()`
- `jdbc2.TimezoneTest :: localTimestampsInPacificNiue()`
- `jdbc2.TimezoneTest :: setDate()`
- `jdbc2.TimezoneTest :: setTime()`
- `jdbc2.TimezoneTest :: setTimestamp()`
- `jdbc2.TimezoneTest :: setTimestampOnTime()`
- `jdbc2.TransactionStateTest :: [2] sql=START TRANSACTION`
- `jdbc2.TypeCacheDLLStressTest :: createDropTableAndGetTypeInfo()`
- `jdbc2.UpdateableResultTest :: simpleAndUpdateableSameQuery()`
- `jdbc2.UpdateableResultTest :: test2193()`
- `jdbc2.UpdateableResultTest :: testArray()`
- `jdbc2.UpdateableResultTest :: testBadColumnIndexes()`
- `jdbc2.UpdateableResultTest :: testCancelRowUpdates()`
- `jdbc2.UpdateableResultTest :: testDeleteRows()`
- `jdbc2.UpdateableResultTest :: testInsertRowIllegalMethods()`
- `jdbc2.UpdateableResultTest :: testInsertRowWithJavaTimeValues()`
- `jdbc2.UpdateableResultTest :: testMultiColumnUpdate()`
- `jdbc2.UpdateableResultTest :: testMultiColumnUpdateWithoutAllColumns()`
- `jdbc2.UpdateableResultTest :: testNoUniqueNotUpdateable()`
- `jdbc2.UpdateableResultTest :: testOidUpdatable()`
- `jdbc2.UpdateableResultTest :: testPrimaryAndUniqueUpdateableByPrimary()`
- `jdbc2.UpdateableResultTest :: testPrimaryAndUniqueUpdateableByUnique()`
- `jdbc2.UpdateableResultTest :: testReturnSerial()`
- `jdbc2.UpdateableResultTest :: testUniqueWithNotNullableColumnUpdateable()`
- `jdbc2.UpdateableResultTest :: testUniqueWithNullAndNotNullableColumnUpdateable()`
- `jdbc2.UpdateableResultTest :: testUniqueWithNullableColumnNotUpdateable()`
- `jdbc2.UpdateableResultTest :: testUniqueWithNullableColumnsNotUpdatable()`
- `jdbc2.UpdateableResultTest :: testUpdateBoolean()`
- `jdbc2.UpdateableResultTest :: testUpdateDate()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithLocalDate()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithLocalDateTime()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithLocalTime()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithOffsetDateTime()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithOffsetTime()`
- `jdbc2.UpdateableResultTest :: testUpdateSelectOnly()`
- `jdbc2.UpdateableResultTest :: testUpdateStreams()`
- `jdbc2.UpdateableResultTest :: testUpdateTimestamp()`
- `jdbc2.UpdateableResultTest :: testUpdateable()`
- `jdbc2.UpdateableResultTest :: testUpdateablePreparedStatement()`
- `jdbc2.UpdateableResultTest :: testUpdateableWithSameTableNameInMultipleSchemas()`
- `jdbc2.UpdateableResultTest :: testZeroRowResult()`

# pgjdbc conformance report

- SecantusDB (Python server) 0.6.0b9
- suite: vendor/pgjdbc @ 3297557c6a80 (Gradle `:postgresql:test`, unmodified; 60s JUnit default timeout injected)
- generated: 2026-08-10 20:10 UTC

| test class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| jdbc2.ArrayTest | 42 | 4 | 0 | 46 | 91.3% |
| jdbc2.AutoRollbackTest | 1048 | 8 | 0 | 1056 | 99.2% |
| jdbc2.AutoSaveTransactionSettingsTest | 0 | 6 | 0 | 6 | 0.0% |
| jdbc2.BatchDeadlockTest | 2 | 6 | 8 | 16 | 25.0% |
| jdbc2.BatchExecuteTest | 132 | 8 | 0 | 140 | 94.3% |
| jdbc2.BatchFailureTest | 136 | 48 | 0 | 184 | 73.9% |
| jdbc2.BatchedInsertReWriteEnabledTest | 56 | 4 | 0 | 60 | 93.3% |
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
| jdbc2.ConnectionTest | 12 | 3 | 0 | 15 | 80.0% |
| jdbc2.CopyLargeFileTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.CopyTest | 23 | 1 | 0 | 24 | 95.8% |
| jdbc2.CursorFetchSqlTransactionTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.CursorFetchTest | 32 | 0 | 0 | 32 | 100.0% |
| jdbc2.CustomTypeWithBinaryTransferTest | 0 | 2 | 0 | 2 | 0.0% |
| jdbc2.DatabaseEncodingTest | 2 | 1 | 0 | 3 | 66.7% |
| jdbc2.DatabaseMetaDataCacheTest | 2 | 1 | 0 | 3 | 66.7% |
| jdbc2.DatabaseMetaDataPropertiesTest | 12 | 1 | 0 | 13 | 92.3% |
| jdbc2.DatabaseMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.DatabaseMetaDataTransactionIsolationTest | 8 | 6 | 0 | 14 | 57.1% |
| jdbc2.DateStyleTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.DateTest | 170 | 8 | 14 | 192 | 95.5% |
| jdbc2.DriverTest | 16 | 0 | 1 | 17 | 100.0% |
| jdbc2.EncodingTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.EnumTest | 0 | 4 | 0 | 4 | 0.0% |
| jdbc2.GeometricTest | 10 | 4 | 0 | 14 | 71.4% |
| jdbc2.GetXXXTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.IntervalTest | 26 | 0 | 0 | 26 | 100.0% |
| jdbc2.JBuilderTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.LoginTimeoutInterruptTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.LoginTimeoutTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.MiscTest | 4 | 0 | 1 | 5 | 100.0% |
| jdbc2.NumericTransfer2Test | 152 | 4 | 0 | 156 | 97.4% |
| jdbc2.NumericTransferTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.OuterJoinSyntaxTest | 0 | 6 | 0 | 6 | 0.0% |
| jdbc2.PGObjectGetTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.PGObjectSetTest | 40 | 0 | 0 | 40 | 100.0% |
| jdbc2.PGPropertyTest | 11 | 0 | 0 | 11 | 100.0% |
| jdbc2.PGTimeTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.PGTimestampTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ParameterStatusTest | 6 | 3 | 0 | 9 | 66.7% |
| jdbc2.PreparedStatementTest | 94 | 12 | 4 | 110 | 88.7% |
| jdbc2.QuotationTest | 2912 | 0 | 0 | 2912 | 100.0% |
| jdbc2.RefCursorFetchTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.RefCursorTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ReplaceProcessingTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ResultSetMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.ResultSetRefreshTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ResultSetTest | 80 | 4 | 0 | 84 | 95.2% |
| jdbc2.SearchPathLookupTest | 0 | 3 | 0 | 3 | 0.0% |
| jdbc2.ServerCursorTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ServerErrorTest | 0 | 7 | 0 | 7 | 0.0% |
| jdbc2.ServerPreparedStmtTest | 13 | 0 | 0 | 13 | 100.0% |
| jdbc2.SharedTimerRefCountTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.SocketTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.StatementTest | 26 | 16 | 0 | 42 | 61.9% |
| jdbc2.StringTypeUnspecifiedArrayTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.TimeTest | 1 | 2 | 0 | 3 | 33.3% |
| jdbc2.TimestampTest | 6 | 8 | 0 | 14 | 42.9% |
| jdbc2.TimezoneCachingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.TimezoneTest | 9 | 7 | 0 | 16 | 56.2% |
| jdbc2.TransactionRoundtripTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.TransactionStateTest | 18 | 1 | 0 | 19 | 94.7% |
| jdbc2.TypeCacheDLLStressTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.UpdateableResultTest | 28 | 8 | 0 | 36 | 77.8% |
| jdbc2.UpsertTest | 32 | 0 | 0 | 32 | 100.0% |
| **total** | **5311** | **219** | **28** | **5558** | **96.0%** |

## Failures (219)

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
- `jdbc2.DatabaseMetaDataPropertiesTest :: values()`
- `jdbc2.DatabaseMetaDataTest :: initializationError`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [1] isolationLevel=8`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [2] isolationLevel=4`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [2] isolationLevel=read uncommitted`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [3] isolationLevel=repeatable read`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [4] isolationLevel=1`
- `jdbc2.DatabaseMetaDataTransactionIsolationTest :: [4] isolationLevel=serializable`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.DateTest :: testGetDate()`
- `jdbc2.EnumTest :: enumArray()`
- `jdbc2.EnumTest :: enumArray()`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.EnumTest :: enumArrayArray()`
- `jdbc2.GeometricTest :: testPGbox()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.GeometricTest :: testPGline()`
- `jdbc2.GeometricTest :: testPGpoint()`
- `jdbc2.LoginTimeoutInterruptTest :: loginTimeoutInterruptsAuthPluginSleep()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.NumericTransfer2Test :: receiveValue()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj2()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithOj3()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithMultipleJoinsAndWithoutOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithSingleJoinAndWithOj()`
- `jdbc2.OuterJoinSyntaxTest :: testOuterJoinSyntaxWithSingleJoinAndWithoutOj()`
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
- `jdbc2.PreparedStatementTest :: testSingleQuotes()`
- `jdbc2.PreparedStatementTest :: testSingleQuotes()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.PreparedStatementTest :: testUnknownSetObject()`
- `jdbc2.RefCursorFetchTest :: initializationError`
- `jdbc2.RefCursorTest :: initializationError`
- `jdbc2.ResultSetMetaDataTest :: initializationError`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.ResultSetTest :: testRowResultPositioning()`
- `jdbc2.ResultSetTest :: testTimestamp()`
- `jdbc2.ResultSetTest :: testTimestamp()`
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
- `jdbc2.StatementTest :: concurrentIsValid()`
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
- `jdbc2.TimezoneTest :: setDate()`
- `jdbc2.TimezoneTest :: setTime()`
- `jdbc2.TimezoneTest :: setTimestamp()`
- `jdbc2.TransactionStateTest :: [2] sql=START TRANSACTION`
- `jdbc2.UpdateableResultTest :: test2193()`
- `jdbc2.UpdateableResultTest :: testArray()`
- `jdbc2.UpdateableResultTest :: testOidUpdatable()`
- `jdbc2.UpdateableResultTest :: testReturnSerial()`
- `jdbc2.UpdateableResultTest :: testUpdateDate()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithLocalDateTime()`
- `jdbc2.UpdateableResultTest :: testUpdateRowWithOffsetDateTime()`
- `jdbc2.UpdateableResultTest :: testUpdateableWithSameTableNameInMultipleSchemas()`

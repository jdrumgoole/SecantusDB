# pgjdbc conformance report

- SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17
- suite: vendor/pgjdbc @ unknown (Gradle `:postgresql:test`, unmodified; 60s JUnit default timeout injected)
- generated: 2026-10-26 07:25 UTC

| test class | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| jdbc2.ArrayTest | 46 | 0 | 0 | 46 | 100.0% |
| jdbc2.AutoRollbackTest | 816 | 240 | 0 | 1056 | 77.2% |
| jdbc2.AutoSaveTransactionSettingsTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.BatchDeadlockTest | 8 | 0 | 8 | 16 | 100.0% |
| jdbc2.BatchExecuteTest | 140 | 0 | 0 | 140 | 100.0% |
| jdbc2.BatchFailureTest | 184 | 0 | 0 | 184 | 100.0% |
| jdbc2.BatchedInsertReWriteEnabledTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.BlobTest | 28 | 0 | 0 | 28 | 100.0% |
| jdbc2.BlobTransactionTest | 0 | 1 | 0 | 1 | 0.0% |
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
| jdbc2.CursorFetchSqlTransactionTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.CursorFetchTest | 32 | 0 | 0 | 32 | 100.0% |
| jdbc2.CustomTypeWithBinaryTransferTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.DatabaseEncodingTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.DatabaseMetaDataCacheTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.DatabaseMetaDataPropertiesTest | 13 | 0 | 0 | 13 | 100.0% |
| jdbc2.DatabaseMetaDataTest | 0 | 1 | 0 | 1 | 0.0% |
| jdbc2.DatabaseMetaDataTransactionIsolationTest | 14 | 0 | 0 | 14 | 100.0% |
| jdbc2.DateStyleTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.DateTest | 178 | 0 | 14 | 192 | 100.0% |
| jdbc2.DriverTest | 16 | 0 | 1 | 17 | 100.0% |
| jdbc2.EncodingTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.EnumTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.GeometricTest | 14 | 0 | 0 | 14 | 100.0% |
| jdbc2.GetXXXTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.IntervalTest | 26 | 0 | 0 | 26 | 100.0% |
| jdbc2.JBuilderTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.LoginTimeoutInterruptTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.LoginTimeoutTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.MiscTest | 4 | 0 | 1 | 5 | 100.0% |
| jdbc2.NumericTransfer2Test | 156 | 0 | 0 | 156 | 100.0% |
| jdbc2.NumericTransferTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.OuterJoinSyntaxTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.PGObjectGetTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.PGObjectSetTest | 40 | 0 | 0 | 40 | 100.0% |
| jdbc2.PGPropertyTest | 11 | 0 | 0 | 11 | 100.0% |
| jdbc2.PGTimeTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.PGTimestampTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ParameterStatusTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.PreparedStatementTest | 106 | 0 | 4 | 110 | 100.0% |
| jdbc2.QuotationTest | 2912 | 0 | 0 | 2912 | 100.0% |
| jdbc2.RefCursorFetchTest | 1536 | 0 | 0 | 1536 | 100.0% |
| jdbc2.RefCursorTest | 8 | 0 | 0 | 8 | 100.0% |
| jdbc2.ReplaceProcessingTest | 9 | 0 | 0 | 9 | 100.0% |
| jdbc2.ResultSetMetaDataTest | 60 | 0 | 0 | 60 | 100.0% |
| jdbc2.ResultSetRefreshTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ResultSetTest | 84 | 0 | 0 | 84 | 100.0% |
| jdbc2.SearchPathLookupTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.ServerCursorTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.ServerErrorTest | 7 | 0 | 0 | 7 | 100.0% |
| jdbc2.ServerPreparedStmtTest | 13 | 0 | 0 | 13 | 100.0% |
| jdbc2.SharedTimerRefCountTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.SocketTimeoutTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.StatementTest | 42 | 0 | 0 | 42 | 100.0% |
| jdbc2.StringTypeUnspecifiedArrayTest | 2 | 0 | 0 | 2 | 100.0% |
| jdbc2.TimeTest | 3 | 0 | 0 | 3 | 100.0% |
| jdbc2.TimestampTest | 14 | 0 | 0 | 14 | 100.0% |
| jdbc2.TimezoneCachingTest | 4 | 0 | 0 | 4 | 100.0% |
| jdbc2.TimezoneTest | 16 | 0 | 0 | 16 | 100.0% |
| jdbc2.TransactionRoundtripTest | 6 | 0 | 0 | 6 | 100.0% |
| jdbc2.TransactionStateTest | 19 | 0 | 0 | 19 | 100.0% |
| jdbc2.TypeCacheDLLStressTest | 1 | 0 | 0 | 1 | 100.0% |
| jdbc2.UpdateableResultTest | 36 | 0 | 0 | 36 | 100.0% |
| jdbc2.UpsertTest | 32 | 0 | 0 | 32 | 100.0% |
| **total** | **6929** | **242** | **28** | **7199** | **96.6%** |

## Failures (242)

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
- `jdbc2.BlobTransactionTest :: initializationError`
- `jdbc2.DatabaseMetaDataTest :: initializationError`

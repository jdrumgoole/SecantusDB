# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.6.0b9
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-08-11 11:00 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 108 | 86 | 22 | 216 | 55.7% |
| pgproto3 | 171 | 1 | 0 | 172 | 99.4% |
| **total** | **291** | **87** | **22** | **400** | **77.0%** |

## Failures (87)

- `pgconn :: TestCancelRequestContextWatcherHandler`
- `pgconn :: TestCancelRequestContextWatcherHandler/DeadlineExceeded_-_do_not_send_cancel_request_when_query_finishes_in_grace_period`
- `pgconn :: TestCancelRequestContextWatcherHandler/DeadlineExceeded_cancels_request_after_CancelRequestDelay`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_0`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_1`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_2`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_3`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_4`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_5`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_6`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_7`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_8`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_9`
- `pgconn :: TestConnCancelRequest`
- `pgconn :: TestConnContextCanceledCancelsRunningQueryOnServer`
- `pgconn :: TestConnContextCanceledCancelsRunningQueryOnServer/postgres`
- `pgconn :: TestConnCopyFrom`
- `pgconn :: TestConnCopyFromBinary`
- `pgconn :: TestConnCopyFromCanceled`
- `pgconn :: TestConnCopyFromDataWriteAfterErrorAndReturn`
- `pgconn :: TestConnCopyFromGzipReader`
- `pgconn :: TestConnCopyFromNoticeResponseReceivedMidStream`
- `pgconn :: TestConnCopyFromPrecanceled`
- `pgconn :: TestConnCopyFromQueryNoTableError`
- `pgconn :: TestConnCopyFromQuerySyntaxError`
- `pgconn :: TestConnCopyToCanceled`
- `pgconn :: TestConnCopyToLarge`
- `pgconn :: TestConnCopyToPrecanceled`
- `pgconn :: TestConnCopyToQueryError`
- `pgconn :: TestConnCopyToSmall`
- `pgconn :: TestConnCustomData`
- `pgconn :: TestConnDeallocate`
- `pgconn :: TestConnDeallocateNonExistentStatementSucceeds`
- `pgconn :: TestConnDeallocateSucceedsInAbortedTransaction`
- `pgconn :: TestConnEscapeString`
- `pgconn :: TestConnExec`
- `pgconn :: TestConnExecBatchDeferredError`
- `pgconn :: TestConnExecBatchImplicitTransaction`
- `pgconn :: TestConnExecBatchPrecanceled`
- `pgconn :: TestConnExecContextPrecanceled`
- `pgconn :: TestConnExecDeferredError`
- `pgconn :: TestConnExecEmpty`
- `pgconn :: TestConnExecMultipleQueries`
- `pgconn :: TestConnExecMultipleQueriesEagerFieldDescriptions`
- `pgconn :: TestConnExecMultipleQueriesError`
- `pgconn :: TestConnExecParams`
- `pgconn :: TestConnExecParamsDeferredError`
- `pgconn :: TestConnExecParamsEmptySQL`
- `pgconn :: TestConnExecParamsMaxNumberOfParams`
- `pgconn :: TestConnExecParamsPrecanceled`
- `pgconn :: TestConnExecParamsTooManyParams`
- `pgconn :: TestConnExecPrepared`
- `pgconn :: TestConnExecPreparedEmptySQL`
- `pgconn :: TestConnExecPreparedMaxNumberOfParams`
- `pgconn :: TestConnExecPreparedPrecanceled`
- `pgconn :: TestConnExecPreparedTooManyParams`
- `pgconn :: TestConnExecStatement`
- `pgconn :: TestConnExecStatementNetworkUsage`
- `pgconn :: TestConnLargeResponseWhileWritingDoesNotDeadlock`
- `pgconn :: TestConnLocking`
- `pgconn :: TestConnOnNotice`
- `pgconn :: TestConnOnNotification`
- `pgconn :: TestConnPrepareContextPrecanceled`
- `pgconn :: TestConnPrepareSyntaxError`
- `pgconn :: TestConnWaitForNotification`
- `pgconn :: TestConnWaitForNotificationPrecanceled`
- `pgconn :: TestConnWaitForNotificationTimeout`
- `pgconn :: TestConnectProtocolVersion32`
- `pgconn :: TestConnectWithRuntimeParams`
- `pgconn :: TestConnectWithValidateConnectTargetSessionAttrsReadWrite`
- `pgconn :: TestDeadlineContextWatcherHandler`
- `pgconn :: TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`
- `pgconn :: TestHijackAndConstruct`
- `pgconn :: TestPipelineCloseReadsUnreadResults`
- `pgconn :: TestPipelineFlushForRequestSeries`
- `pgconn :: TestPipelineFlushForSingleRequests`
- `pgconn :: TestPipelineFlushWithError`
- `pgconn :: TestPipelineGetResultsHandlesPartiallyReadResults`
- `pgconn :: TestPipelinePrepare`
- `pgconn :: TestPipelinePrepareAndDeallocate`
- `pgconn :: TestPipelinePrepareError`
- `pgconn :: TestPipelinePrepareQuery`
- `pgconn :: TestPipelineQuery`
- `pgconn :: TestPipelineQueryErrorBetweenSyncs`
- `pgconn :: TestResultReaderReadNil`
- `pgconn :: TestResultReaderValuesHaveSameCapacityAsLength`
- `pgproto3 :: TestTrace`

# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.6.0b11
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-08-15 14:39 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 180 | 14 | 22 | 216 | 92.8% |
| pgproto3 | 172 | 0 | 0 | 172 | 100.0% |
| **total** | **364** | **14** | **22** | **400** | **96.3%** |

## Failures (14)

- `pgconn :: TestCancelRequestContextWatcherHandler`
- `pgconn :: TestCancelRequestContextWatcherHandler/DeadlineExceeded_cancels_request_after_CancelRequestDelay`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_0`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_3`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_4`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_5`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_6`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_8`
- `pgconn :: TestCancelRequestContextWatcherHandler/Stress_9`
- `pgconn :: TestConnContextCanceledCancelsRunningQueryOnServer`
- `pgconn :: TestConnContextCanceledCancelsRunningQueryOnServer/postgres`
- `pgconn :: TestConnCopyFromNoticeResponseReceivedMidStream`
- `pgconn :: TestDeadlineContextWatcherHandler`
- `pgconn :: TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`

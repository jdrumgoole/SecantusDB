# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.7.0b3
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-10-19 06:39 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 190 | 4 | 22 | 216 | 97.9% |
| pgproto3 | 172 | 0 | 0 | 172 | 100.0% |
| **total** | **374** | **4** | **22** | **400** | **98.9%** |

## Failures (4)

- `pgconn :: TestCancelRequestContextWatcherHandler`
- `pgconn :: TestCancelRequestContextWatcherHandler/DeadlineExceeded_cancels_request_after_CancelRequestDelay`
- `pgconn :: TestDeadlineContextWatcherHandler`
- `pgconn :: TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`

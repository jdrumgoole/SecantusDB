# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.6.0b11
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-08-15 15:58 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 192 | 2 | 22 | 216 | 99.0% |
| pgproto3 | 172 | 0 | 0 | 172 | 100.0% |
| **total** | **376** | **2** | **22** | **400** | **99.5%** |

## Failures (2)

- `pgconn :: TestDeadlineContextWatcherHandler`
- `pgconn :: TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`

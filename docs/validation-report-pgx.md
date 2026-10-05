# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.6.0b17
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-10-05 06:46 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 192 | 2 | 22 | 216 | 98.9% |
| pgproto3 | 172 | 0 | 0 | 172 | 100.0% |
| **total** | **376** | **2** | **22** | **400** | **99.4%** |

## Failures (2)

- `pgconn :: TestDeadlineContextWatcherHandler`
- `pgconn :: TestDeadlineContextWatcherHandler/DeadlineExceeded_with_DeadlineDelay`

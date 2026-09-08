# pgx (pgconn + pgproto3) conformance report

- SecantusDB (Python server) 0.6.0b16
- suite: vendor/pgx @ 0aeabbcf11d8 (`go test`, unmodified)
- generated: 2026-08-31 06:38 UTC

| package | passed | failed | skipped | total | pass rate |
|---|---|---|---|---|---|
| bgreader | 6 | 0 | 0 | 6 | 100.0% |
| ctxwatch | 6 | 0 | 0 | 6 | 100.0% |
| pgconn | 193 | 1 | 22 | 216 | 99.5% |
| pgproto3 | 172 | 0 | 0 | 172 | 100.0% |
| **total** | **377** | **1** | **22** | **400** | **99.7%** |

## Failures (1)

- `pgconn :: TestConnExecBatchHuge`

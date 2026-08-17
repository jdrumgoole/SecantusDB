# pgbench + psql stress/smoke report

- SecantusDB (Python server) 0.6.0b11
- pgbench TPC-B (simple / extended / prepared) + select-only + psql catalog smoke
- generated: 2026-08-17 06:20 UTC

**0/6 lanes clean.** Any error or dropped connection is a bug;
tps figures are smoke-level indicators, not benchmarks.

| lane | status | tps |
|---|---|---|
| init (-i -s 1) | FAIL — pgbench: error: unexpected copy in result: ERROR:  syntax error at or near "on" | — |
| tpcb -M simple (c=1 t=50) | FAIL — pgbench: error: Run was aborted; the above results are incomplete. | — |
| tpcb -M extended (c=1 t=50) | FAIL — pgbench: error: Run was aborted; the above results are incomplete. | — |
| tpcb -M prepared (c=1 t=50) | FAIL — pgbench: error: Run was aborted; the above results are incomplete. | — |
| select-only (c=4 t=100) | FAIL — pgbench: error: Run was aborted; the above results are incomplete. | — |
| psql catalog smoke | FAIL —                  ^ | — |

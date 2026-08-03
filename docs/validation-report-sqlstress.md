# pgbench + psql stress/smoke report

- SecantusDB (Python server) 0.6.0b9
- pgbench TPC-B (simple / extended / prepared) + select-only + psql catalog smoke
- generated: 2026-08-03 07:05 UTC

**5/6 lanes clean.** Any error or dropped connection is a bug;
tps figures are smoke-level indicators, not benchmarks.

| lane | status | tps |
|---|---|---|
| init (-i -s 1) | ok | — |
| tpcb -M simple (c=1 t=50) | ok | 178 |
| tpcb -M extended (c=1 t=50) | ok | 3 |
| tpcb -M prepared (c=1 t=50) | ok | 3 |
| select-only (c=4 t=100) | ok | 911 |
| psql catalog smoke | FAIL —                  ^ | — |

# pgbench + psql stress/smoke report

- SecantusDB (Python server) 0.6.0b6
- pgbench TPC-B (simple / extended / prepared) + select-only + psql catalog smoke
- generated: 2026-07-31 17:43 UTC

**6/6 lanes clean.** Any error or dropped connection is a bug;
tps figures are smoke-level indicators, not benchmarks.

| lane | status | tps |
|---|---|---|
| init (-i -s 1) | ok | — |
| tpcb -M simple (c=1 t=50) | ok | 210 |
| tpcb -M extended (c=1 t=50) | ok | 156 |
| tpcb -M prepared (c=1 t=50) | ok | 209 |
| select-only (c=4 t=100) | ok | 1385 |
| psql catalog smoke | ok | — |

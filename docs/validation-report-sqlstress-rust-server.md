# pgbench + psql stress/smoke report

- SecantusDB (Rust PostgreSQL server, `secantusd-pg`) 0.1.0-beta.17
- pgbench TPC-B (simple / extended / prepared) + select-only + psql catalog smoke
- generated: 2026-10-26 06:48 UTC

**6/6 lanes clean.** Any error or dropped connection is a bug;
tps figures are smoke-level indicators, not benchmarks.

| lane | status | tps |
|---|---|---|
| init (-i -s 1) | ok | — |
| tpcb -M simple (c=1 t=50) | ok | 61 |
| tpcb -M extended (c=1 t=50) | ok | 48 |
| tpcb -M prepared (c=1 t=50) | ok | 55 |
| select-only (c=4 t=100) | ok | 3597 |
| psql catalog smoke | ok | — |

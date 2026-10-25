# Isolation specs

`*.spec` are PostgreSQL's isolation tests, from `src/test/isolation/specs`
at tag `REL_15_19`, unchanged, except the four whose header says they were
written for SecantusDB (`on-conflict-where-lock`, `snapshot-at-first-query`,
`unique-insert-wait`, `unique-column-insert-wait`).
PostgreSQL is Copyright (c) 1996-2022, The PostgreSQL Global Development
Group, and Portions Copyright (c) 1994, The Regents of the University of
California, under the PostgreSQL License.

`*.json` are PostgreSQL 15.19's traces of each spec's permutations,
recorded on a private cluster with
`tools/probes/pg_isolation.py SPEC... --a DSN --wait 2 --record DIR`.
`tests/backlog/test_rp_isolation.py` replays them against `secantusd-pg`.

### Driver gauges re-run against the Rust MongoDB server; the smoke test checks the binary it runs

The thirteen driver gauges were re-run against the Rust MongoDB server on the
tree that now includes the planner work and exact numeric comparison, and the
reports and the site's driver panels were regenerated from that run.

#### Fixed

- `tests/test_rust_binary_smoke.py` smoked a stale release binary ahead of
  the debug binary the provenance check had verified, so an old build's
  failure read as a regression. It now picks a binary built from `HEAD`, and
  otherwise only the debug binary that check covers (closes RM-112; release
  tooling, no gap test).

#### Changed

- Backlog: RM-113 (the release skill's `timeout`) closed, already fixed in
  the skill; RM-027 narrowed to `mapReduce` with `out`, since `compact` and
  `setParameter notablescan` now match mongod 8.2.11.

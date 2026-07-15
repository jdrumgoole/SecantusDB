### CI: the cross-driver summary regenerates with the weekly validate run

`docs/validation-summary.md` had been frozen since 2026-06-20 ("the 11
gauges") while the per-driver reports refreshed weekly. Each gauge job now
uploads its raw output (`.validation/`) as an artifact alongside its
report, and the aggregate job reassembles them and regenerates the summary
in the same refresh PR — no WiredTiger build needed there, because the
generator now reads the package version straight from `src/` and resolves
vendored-driver SHAs from the superproject's gitlinks (`git ls-tree`)
instead of requiring checked-out submodules.

#### Added

- `validation_summary.generate`: collectors for the **mongo-kotlin-driver**
  gauge (JUnit XML from `:driver-kotlin-sync:integrationTest`) and the
  **pymongo (async)** gauge (`AsyncMongoClient` suite), bringing the
  summary to 13 gauges; the gauge count in the prose is computed, not
  hand-written.

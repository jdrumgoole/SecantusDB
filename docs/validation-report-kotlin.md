# mongo-kotlin-driver Validation Report

Generated 2026-08-17 — SecantusDB 0.6.0b11 vs mongo-kotlin-driver cb45be6bb147 (`vendor/mongo-java-driver/driver-kotlin-sync/`).

Run `uv run python -m invoke validate-kotlin` to refresh. This is the official MongoDB **Kotlin** driver gauge — the Kotlin sync client (which ships in the mongo-java-driver monorepo) exercised end-to-end against a standalone SecantusDB daemon.

## Scope

`driver-kotlin-sync/src/integrationTest/` contains **3** test classes upstream. The gauge currently runs **2** of them (~67%). The rest are either out of scope (search-index / change-stream-resume / retryable scenarios that need features SecantusDB doesn't implement) or unaudited — each new class needs the runner's wall-clock guard to confirm it terminates before it's added to `kotlin_validation/include_modules.py`. The pass rate below describes the included subset, not the whole integration tree.

## Summary by module

| Module | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `driver-kotlin-sync` | 294 | 0 | 244 | 538 | 100.0% |
| **Overall** | **294** | **0** | **244** | **538** | **100.0%** |

## How this is generated

**mongo-kotlin-driver's integration tests are run unmodified, against a standalone SecantusDB daemon.** The Kotlin driver lives in the mongo-java-driver monorepo (`vendor/mongo-java-driver/driver-kotlin-sync/`), checked out at the pinned upstream tag with zero local edits. `kotlin_validation/runner.py` does the same two-phase spawn as the Java gauge: phase 1 boots `python -m secantus --port 0 --storage-path <tempdir> --standalone` without `--auth` and seeds `root-user` (root role) via pymongo; phase 2 restarts on the same tempdir **with `--auth`**. Gradle then runs the bundled wrapper's `:driver-kotlin-sync:integrationTest` task with `-Dorg.mongodb.test.uri=mongodb://root-user:password@…/?authSource=admin`.

These are **integration tests** under `driver-kotlin-sync/src/integrationTest/` — every test opens a real TCP connection through the Kotlin client (over its `syncadapter` shim), SCRAM-authenticates, and exchanges wire commands. The pure-Mockito unit tests under `src/test/` never touch a server and are intentionally excluded. The driver writes JUnit XML to `<module>/build/test-results/integrationTest/TEST-*.xml`; we copy those out of the vendored tree (so the submodule stays untouched) and parse them here. Widen `kotlin_validation/include_modules.py` to add more classes.

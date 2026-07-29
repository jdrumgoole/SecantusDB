# mongo-php-driver Validation Report

Generated 2026-07-27 — SecantusDB 0.6.0b1 vs mongo-php-driver e81b318a33dc (`vendor/mongo-php-driver/`).

Run `uv run python -m invoke validate-php-ext` to refresh. This is the low-level PHP extension (the PECL `mongodb` package that wraps libmongoc) — the strictest wire-protocol gauge, alongside mongo-go-driver, for catching bugs pymongo's permissive client misses.

## Summary by category

| Category | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `tests/bson` | 424 | 0 | 18 | 442 | 100.0% |
| `tests/bulk` | 43 | 0 | 2 | 45 | 100.0% |
| `tests/command` | 10 | 0 | 1 | 11 | 100.0% |
| `tests/cursor` | 57 | 0 | 5 | 62 | 100.0% |
| `tests/exception` | 11 | 0 | 1 | 12 | 100.0% |
| `tests/functional` | 6 | 0 | 0 | 6 | 100.0% |
| `tests/query` | 24 | 0 | 0 | 24 | 100.0% |
| `tests/readConcern` | 15 | 0 | 1 | 16 | 100.0% |
| `tests/readPreference` | 29 | 0 | 2 | 31 | 100.0% |
| `tests/writeConcern` | 25 | 0 | 3 | 28 | 100.0% |
| `tests/writeConcernError` | 1 | 0 | 4 | 5 | 100.0% |
| `tests/writeError` | 7 | 0 | 0 | 7 | 100.0% |
| `tests/writeResult` | 19 | 0 | 4 | 23 | 100.0% |
| **Overall** | **671** | **0** | **41** | **712** | **100.0%** |

Run time: 41.11s.

## How this is generated

**mongo-php-driver's `.phpt` suite is run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-php-driver/` is checked out at the pinned upstream tag (matching the installed extension version) with zero local edits. `php_ext_validation/runner.py` boots `python -m secantus --storage-path <tempdir>`, then runs PHP's `run-tests.php` over the curated directories in `include_paths.py` against the **already-installed** `mongodb` extension — no rebuild — with `MONGODB_URI=mongodb://127.0.0.1:<port>/` (the var `tests/utils/basic.inc` reads) and `TEST_PHP_JUNIT` pointing at the JUnit output. The on-disk tempdir is removed after the run.

The `tests/bson` directory is pure-driver BSON serialization (no server); every other included directory opens a real TCP connection and exchanges wire commands end-to-end. The `.phpt` files self-guard by topology via `skip_if_*` helpers, so tests that need a replica set, transactions, or CSFLE SKIP cleanly. The include set excludes the huge `bson-corpus` directory and the orchestration-dependent suites (`session`, `retryable-*`, `replicaset`, `clientEncryption`) rather than counting them as skips.

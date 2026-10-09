# mongo-php-library Validation Report

Measured 2026-10-09 (raw artifact), generated 2026-10-09 — SecantusDB 0.7.0b2 vs mongo-php-library 12e56461166d (`vendor/mongo-php-library/`).

Run `uv run python -m invoke validate-php-lib` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges for the official high-level PHP library — the `mongodb/mongodb` package Laravel + Symfony applications build on.

## Summary by category

| Category | Passed | Failed | Expected | Skipped | Total | Pass rate | Adjusted |
|---|---:|---:|---:|---:|---:|---:|---:|
| `tests/Builder` | 732 | 0 | 0 | 0 | 732 | 100.0% | 100.0% |
| `tests/Collection` | 159 | 0 | 0 | 1 | 160 | 100.0% | 100.0% |
| `tests/Command` | 53 | 0 | 0 | 0 | 53 | 100.0% | 100.0% |
| `tests/Comparator` | 31 | 0 | 0 | 0 | 31 | 100.0% | 100.0% |
| `tests/Database` | 70 | 0 | 0 | 0 | 70 | 100.0% | 100.0% |
| `tests/Functions` | 0 | 0 | 0 | 4 | 4 | — | — |
| `tests/Model` | 141 | 0 | 1 | 0 | 142 | 99.2% | 100.0% |
| `tests/Operation` | 1915 | 1 | 0 | 22 | 1938 | 99.9% | 99.9% |
| **Overall** | **3101** | **1** | **1** | **27** | **3130** | **99.9%** | **99.9%** |

Run time: 8.11s.

## Failures (1)

First 30 failed cases for triage:

```
tests/Operation :: MongoDB\Tests\Operation\WatchFunctionalTest::testOriginalReadPreferenceIsPreservedOnResume
```

## Expected failures (1)

Documented gaps, declared in `validation_summary/expected_failures.py`. They still FAILED — the gauge is not told to skip them — and they are counted in the plain pass rate. The adjusted column is the same number with them removed from the denominator.

- `tests/Model :: MongoDB\Tests\Model\IndexInfoFunctionalTest::testIsText` — Text indexes (`$text`, `$meta: textScore`, text-index creation) are intentionally out of scope per CLAUDE.md — would require a full-text index implementation. The driver's own error names it: `text indexes are not supported by SecantusDB`. Documented in tasks/backlog.md §4. The SAME gap is already declared for the node and pymongo gauges; php-library was the one left reading as an unexplained failure.

## How this is generated

**mongo-php-library's PHPUnit suite is run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-php-library/` is checked out at the pinned upstream tag with zero local edits. `php_lib_validation/runner.py` runs `composer install` (one-time per checkout) to materialise PHPUnit, boots `python -m secantus --storage-path <tempdir>`, then runs `vendor/bin/phpunit --log-junit <xml>` over the curated functional directories in `include_paths.py` with `MONGODB_URI=mongodb://127.0.0.1:<port>/` and `MONGODB_DATABASE=phplib_test` — the env vars `tests/TestCase.php` reads. The on-disk tempdir is removed after the run.

Every functional test opens a real TCP connection to the SecantusDB daemon and exchanges wire commands end-to-end, so the pass rate measures SecantusDB's compatibility with the PHP library, not the library's own pure-code logic. The include set is narrow on purpose: the spec-corpus suites (`SpecTests` / `UnifiedSpecTests`), GridFS, and the documentation-example tests need replica-set / transaction / CSFLE orchestration SecantusDB doesn't provide, so they're excluded rather than counted as environment-gated skips.

# mongo-php-library Validation Report

Generated 2026-06-15 — SecantusDB 0.5.3b7 vs mongo-php-library unknown (`vendor/mongo-php-library/`).

Run `uv run python -m invoke validate-php-lib` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges for the official high-level PHP library — the `mongodb/mongodb` package Laravel + Symfony applications build on.

## Summary by category

| Category | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `tests/Builder` | 732 | 0 | 0 | 732 | 100.0% |
| `tests/Collection` | 125 | 34 | 1 | 160 | 78.6% |
| `tests/Command` | 53 | 0 | 0 | 53 | 100.0% |
| `tests/Comparator` | 31 | 0 | 0 | 31 | 100.0% |
| `tests/Database` | 69 | 1 | 0 | 70 | 98.6% |
| `tests/Functions` | 0 | 2 | 2 | 4 | 0.0% |
| `tests/Model` | 140 | 2 | 0 | 142 | 98.6% |
| `tests/Operation` | 1879 | 23 | 36 | 1938 | 98.8% |
| **Overall** | **3029** | **62** | **39** | **3130** | **98.0%** |

Run time: 3.03s.

## Failures (62)

First 30 failed cases for triage:

```
tests/Operation :: MongoDB\Tests\Operation\AggregateFunctionalTest::testExplainOption
tests/Operation :: MongoDB\Tests\Operation\AggregateFunctionalTest::testExplainOptionWithWriteConcern
tests/Operation :: MongoDB\Tests\Operation\AggregateFunctionalTest::testReadPreferenceWithinTransaction
tests/Operation :: MongoDB\Tests\Operation\BulkWriteFunctionalTest::testInserts
tests/Operation :: MongoDB\Tests\Operation\CountFunctionalTest::testHintOption
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testCount with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testDelete with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testDeleteMany with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testDeleteOne with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testDistinct with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFindAndModify with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFind with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFindOne with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFindOneAndDelete with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFindOneAndReplace with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testFindOneAndUpdate with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testUpdate with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testUpdateMany with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testUpdateOne with data set #0
tests/Operation :: MongoDB\Tests\Operation\ExplainFunctionalTest::testAggregateOptimizedToQuery with data set #0
tests/Operation :: MongoDB\Tests\Operation\FindFunctionalTest::testReadPreferenceWithinTransaction
tests/Operation :: MongoDB\Tests\Operation\ModifyCollectionFunctionalTest::testCollMod
tests/Operation :: MongoDB\Tests\Operation\WatchFunctionalTest::testResumeTokenInvalidTypeServerSideError
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testAggregateWithinTransaction
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testFindWithinTransaction
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "read-only aggregate"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "bulkWrite insertOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "countDocuments"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "deleteMany"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "deleteOne"
```
... and 32 more (see JUnit XML).

## How this is generated

**mongo-php-library's PHPUnit suite is run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-php-library/` is checked out at the pinned upstream tag with zero local edits. `php_lib_validation/runner.py` runs `composer install` (one-time per checkout) to materialise PHPUnit, boots `python -m secantus --storage-path <tempdir>`, then runs `vendor/bin/phpunit --log-junit <xml>` over the curated functional directories in `include_paths.py` with `MONGODB_URI=mongodb://127.0.0.1:<port>/` and `MONGODB_DATABASE=phplib_test` — the env vars `tests/TestCase.php` reads. The on-disk tempdir is removed after the run.

Every functional test opens a real TCP connection to the SecantusDB daemon and exchanges wire commands end-to-end, so the pass rate measures SecantusDB's compatibility with the PHP library, not the library's own pure-code logic. The include set is narrow on purpose: the spec-corpus suites (`SpecTests` / `UnifiedSpecTests`), GridFS, and the documentation-example tests need replica-set / transaction / CSFLE orchestration SecantusDB doesn't provide, so they're excluded rather than counted as environment-gated skips.

# mongo-php-library Validation Report

Generated 2026-07-27 — SecantusDB 0.6.0b1 vs mongo-php-library 12e56461166d (`vendor/mongo-php-library/`).

Run `uv run python -m invoke validate-php-lib` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver / mongo-ruby-driver gauges for the official high-level PHP library — the `mongodb/mongodb` package Laravel + Symfony applications build on.

## Summary by category

| Category | Passed | Failed | Skipped | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `tests/Builder` | 732 | 0 | 0 | 732 | 100.0% |
| `tests/Collection` | 124 | 35 | 1 | 160 | 78.0% |
| `tests/Command` | 53 | 0 | 0 | 53 | 100.0% |
| `tests/Comparator` | 31 | 0 | 0 | 31 | 100.0% |
| `tests/Database` | 70 | 0 | 0 | 70 | 100.0% |
| `tests/Functions` | 0 | 2 | 2 | 4 | 0.0% |
| `tests/Model` | 141 | 1 | 0 | 142 | 99.3% |
| `tests/Operation` | 1898 | 4 | 36 | 1938 | 99.8% |
| **Overall** | **3049** | **42** | **39** | **3130** | **98.6%** |

Run time: 3.89s.

## Failures (42)

First 30 failed cases for triage:

```
tests/Operation :: MongoDB\Tests\Operation\AggregateFunctionalTest::testReadPreferenceWithinTransaction
tests/Operation :: MongoDB\Tests\Operation\FindFunctionalTest::testMaxAwaitTimeMS
tests/Operation :: MongoDB\Tests\Operation\FindFunctionalTest::testReadPreferenceWithinTransaction
tests/Operation :: MongoDB\Tests\Operation\WatchFunctionalTest::testResumeTokenInvalidTypeServerSideError
tests/Collection :: MongoDB\Tests\Collection\CodecCollectionFunctionalTest::testFindOneAndReplace with data set "BSON type map"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testAggregateWithinTransaction
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testFindWithinTransaction
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "read-only aggregate"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "bulkWrite insertOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "countDocuments"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "deleteMany"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "deleteOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "distinct"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "find"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "findOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "findOneAndDelete"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "findOneAndReplace"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "findOneAndUpdate"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "insertMany"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "insertOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "replaceOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "updateMany"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodDoesNotInheritReadWriteConcernInTransaction with data set "updateOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "bulkWrite insertOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "deleteMany"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "deleteOne"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "findOneAndDelete"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "findOneAndReplace"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "findOneAndUpdate"
tests/Collection :: MongoDB\Tests\Collection\CollectionFunctionalTest::testMethodInTransactionWithWriteConcernOption with data set "insertMany"
```
... and 12 more (see JUnit XML).

## How this is generated

**mongo-php-library's PHPUnit suite is run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-php-library/` is checked out at the pinned upstream tag with zero local edits. `php_lib_validation/runner.py` runs `composer install` (one-time per checkout) to materialise PHPUnit, boots `python -m secantus --storage-path <tempdir>`, then runs `vendor/bin/phpunit --log-junit <xml>` over the curated functional directories in `include_paths.py` with `MONGODB_URI=mongodb://127.0.0.1:<port>/` and `MONGODB_DATABASE=phplib_test` — the env vars `tests/TestCase.php` reads. The on-disk tempdir is removed after the run.

Every functional test opens a real TCP connection to the SecantusDB daemon and exchanges wire commands end-to-end, so the pass rate measures SecantusDB's compatibility with the PHP library, not the library's own pure-code logic. The include set is narrow on purpose: the spec-corpus suites (`SpecTests` / `UnifiedSpecTests`), GridFS, and the documentation-example tests need replica-set / transaction / CSFLE orchestration SecantusDB doesn't provide, so they're excluded rather than counted as environment-gated skips.

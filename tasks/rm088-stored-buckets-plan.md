# RM-088: stored time-series buckets with mongod's bucket catalog

Decided 2026-10-16 (Joe): store real BSONColumn buckets in
`system.buckets.<name>`, kept by a server-wide bucket catalog that models
mongod's memory accounting, in place of measurements stored as documents.
Existing stores are converted on open.

Reference: mongod 8.2.11 source, `src/mongo/db/timeseries/bucket_catalog/`
and `src/mongo/bson/column/` (copies in `.scratch/mongo-src/`, not committed).

## What mongod does (8.2.11, read from source)

- **Bucket size** (`bucket.size`): the meta field once (`"meta"` + value),
  12 + name length + 2 x element size per new field name, plus the
  compressed column bytes after each commit (`MeasurementMap::intermediate`:
  the sum of every column's length). Before a commit, only elements over
  `timeseriesLargeMeasurementThreshold` (32 bytes) are estimated, at their
  uncompressed size.
- **Rollover** (`determineRolloverReason`), in order: time forward (span),
  time backward, count (1000), size, schema change. Size:
  `getCacheDerivedBucketMaxSize(cacheBytes, numActiveBuckets)` =
  min(128000, max(cache / (2 x active), 5120)). Over it, a bucket of fewer
  than 10 measurements stays open up to min(12 MB, the derived size):
  `kSize` or `kCachePressure`.
- **Reopening**: archived buckets in memory (time backward), else a query on
  `system.buckets` (not closed, meta equal, window holds the time,
  `data.<time>.999` missing, `$bsonSize` < max size), first match.
- **Builder reopen** is the same as decoding every value and appending it
  again (`bsoncolumn_fuzzer_impl.cpp` asserts identical state), so the
  decoder plus our builder gives mongod's bytes after a reopen.
- **Stats**: per collection and global `ExecutionStats` (`numBucketInserts`,
  `numBucketUpdates`, `numBucketsClosedDueTo*`, `numBucketsArchivedDueTo*`,
  `numMeasurementsCommitted`, ...), in memory, lost on restart.

## Phases (one PR each, each leaves the server correct)

1. [ ] **BSONColumn decoder** (`secantus_core::bsoncolumn::decode`): a port
   of `BSONColumn::Iterator` and `Simple8b::Iterator`, regular and
   interleaved (0xF0 / 0xF1 / 0xF2). Round-trip tests over the gap test's
   value series, and mongod's bytes.
2. [ ] **The flip**: buckets stored in `system.buckets.<name>` (clustered by
   `_id`), the collection a view (`$_internalUnpackBucket`, implemented);
   inserts through a server-wide `BucketCatalog` (rollover reasons
   including size, archive and query reopening, execution stats); reads
   unpack with the bucket predicate pushed down (closes the whole-collection
   read); indexes stored on the buckets; listCollections / validate /
   collStats sizes / dbStats from the stored buckets; oplog entries on the
   bucket namespace; old stores converted on open.
3. [ ] **Writes**: updates and deletes on the collection rewrite buckets;
   direct insert / update / delete on `system.buckets.<name>` (the catalog
   forgets a bucket written by hand, as mongod's state registry does);
   `renameCollection` of the bucket namespace; findAndModify.
4. [ ] **Counters**: `collStats` / `$collStats` `timeseries` counters and
   `serverStatus.bucketCatalog` from the catalog's execution stats.

Found while mapping (to file, not in this work): a time-series collection's
collection-level `expireAfterSeconds` is stored but never expires anything
(`prune_ttl` only handles clustered collections).

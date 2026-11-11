### Time-series collections are stored as buckets, placed by mongod's bucket catalog

The Rust MongoDB server now keeps a time-series collection the way mongod
does: as bucket documents in `system.buckets.<name>`, each data field in
mongod's compressed column format. The collection itself is a view that
unpacks them. Until now the server stored each measurement as an ordinary
document and worked out what the buckets would have been whenever something
asked. That got the common cases right, but a bucket never closed for size,
a bucket could not be written by hand, and statistics that depend on what
the server did along the way, such as how many buckets it reopened or closed
and why, had nothing to count.

Where each measurement goes is decided by a transcription of mongod 8.2.11's
bucket catalog, kept in memory across the whole server as mongod keeps it.
An insert batch is split by meta value and each part sorted by time. A
measurement goes into an open bucket of its series that can take it,
otherwise into one reopened from memory or found by mongod's reopening
query, otherwise into a new bucket. A bucket rolls over for the same
reasons and in the same order as mongod's: time running forward or back, a
thousand measurements, a change of schema, or its size. Size uses mongod's
own accounting: compressed column bytes after each write, values over 32
bytes counted whole before it, and a limit of 128,000 bytes that shrinks
when the server holds more buckets than its storage cache can keep two of.
A restart forgets the catalog, as mongod's does, so the next measurement is
placed by the reopening query. Stores written by earlier versions are
converted to buckets when they are opened.

Deletes rewrite or remove the buckets they reach, meta updates change the
buckets' `meta`, TTL expires whole buckets once their newest possible
measurement has expired, and a bucket written by hand through
`system.buckets.<name>` is dropped from the catalog. `collStats` reports
mongod's bucket catalog counters. Over the 191-scenario time-series probe
the server now differs from mongod on two scenarios, down from five.

#### Added

- `secantus_core::bsoncolumn::decode`: a port of mongod's
  `BSONColumn::Iterator` and `Simple8b::Iterator`, covering regular and
  interleaved columns (control bytes 0xF0, 0xF1 and 0xF2). A reopened bucket
  decodes its columns and appends every value again, which mongod's own
  fuzzer shows leaves a builder in the same state as reopening it from its
  bytes.
- `secantus-commands/src/bucket_catalog.rs`: mongod's bucket catalog
  (`findOpenBucketForMeasurement`, `potentiallyReopenBucket`,
  `determineRolloverReason`, `getCacheDerivedBucketMaxSize`,
  `rehydrateBucket`) and its execution statistics. The storage adapter holds
  one catalog per server.
- `secantus-commands/src/ts_store.rs`: a time-series collection's reads
  (buckets in `_id` order, filtered at the bucket level, then unpacked),
  deletes, meta updates and `findAndModify` removes, plus
  `upgrade_time_series`, which converts an older store when it is opened.
- `collStats` and `$collStats` report a time-series collection's sizes from
  its buckets, with mongod's `timeseries` section: `bucketsNs`,
  `bucketCount`, `avgBucketSize` and the catalog's counters.

#### Fixed

- Buckets close for size, and a bucket of fewer than ten measurements stays
  open for large values, as on mongod 8.2.11.
- `update`, `delete` and `findAndModify` on `system.buckets.<name>` run, as
  on mongod, where this server answered 115. An insert of a valid bucket
  document is stored.
- Deletes on a time-series collection remove measurements where they
  removed none, and a meta-field `update` changes them.
- `dbStats` counts a time-series collection as a view and its bucket
  collection as a collection, as mongod does.
- TTL indexes and a collection's own `expireAfterSeconds` expire a
  time-series collection's buckets, cut off at `expireAfterSeconds` plus
  `bucketMaxSpanSeconds`. A collection-level `expireAfterSeconds` was
  stored and never expired anything.
- A clustered collection's documents come back in `_id` order from an
  unsorted `find`, `aggregate` or `$lookup`, where they came back in the
  order they were inserted.
- `validate` and `listCollections` name the bucket collection's real UUID.
- Fixes RM-088 (tests/backlog/test_rm_085_112.py::test_bucket_collection_can_be_deleted_from),
  pinned also by `tests/mongod_live/test_time_series_buckets.py` against
  mongod 8.2 and `crates/secantus-storage-adapter/tests/timeseries_catalog_wt.rs`
  (placement, restart, conversion).

#### Changed

- A time-series collection's index specs stay on the collection and are
  not yet built over the buckets, so a read scans the buckets in `_id`
  order and `collStats` reports the index sizes as 0 (RM-457).
- Filed RM-548: `listIndexes` of `system.buckets.<name>` shows a TTL or
  partial index as the collection keeps it, without mongod's single-field
  key, rewritten filter and `originalSpec`.
- Filed RM-549 (decision needed): `serverStatus` has no `bucketCatalog`
  section, and idle buckets never close under memory pressure. Both depend
  on the bytes mongod's own structures allocate.

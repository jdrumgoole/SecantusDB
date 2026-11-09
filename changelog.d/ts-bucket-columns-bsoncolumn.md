### Time-series buckets hold mongod's compressed column bytes

A time-series collection's buckets, as read from `system.buckets.<name>`, now
hold each data field in the compressed bytes mongod writes. Before, the Rust
MongoDB server wrote every value as a literal. That was a valid column, and
mongod could unpack it, but the bytes were not mongod's, and anything that
compared or hashed bucket documents could see the difference. The new
encoder transcribes mongod 8.2.11's BSONColumn builder: delta and
delta-of-delta runs packed into Simple-8b blocks, run-length encoding,
doubles at the smallest decimal scale that holds them (rescaled as the run
grows), 128-bit deltas for strings, binaries and decimals, and interleaved
streams for objects and arrays. It is also the first piece of the decided
plan to store real buckets (RM-088).

#### Fixed

- Bucket `data` columns are mongod's bytes. The encoder is
  `secantus_core::bsoncolumn`, which `ts_buckets` uses for every derived
  bucket. Its order of decisions is mongod's: when a run ends, when a double
  is rescaled, and in which order an interleaved object's control blocks
  are written.
- Checked against mongod 8.2.11 with a matrix of 40 value series covering
  every branch of the builder, all of them equal. A local randomized run of
  1,900 series of every type, with skips and repeats, also found no
  difference.
- RM-088 stays open for the rest of the decided work: stored buckets, the
  server-wide catalog, and bucket writes. Fixes RM-088 in part
  (tests/backlog/test_rm_085_112.py::test_bucket_columns_hold_mongods_bytes).
- Filed RM-462: mongod refuses a measurement that holds a BSONColumn binary
  (12506300), but this server stores it.

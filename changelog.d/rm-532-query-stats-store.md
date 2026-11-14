### `$queryStats` sizes its store, samples, records explains and hashes identifiers

The Rust MongoDB server's query stats store is now mongod's: a partitioned
least-recently-used cache whose budget is `internalQueryStatsCacheSize`
(1% of the machine's memory by default, capped at 1 GB and at a quarter of
memory), split evenly across one partition per 16 MB of budget and at least
one per logical core. A partition over its share evicts its least recently
used entries; shrinking the budget evicts at once, and a budget of 0 bytes
turns query stats off, so `$queryStats` fails with mongod's 6579000.
`internalQueryStatsSampleRate` samples that fraction of queries in place
of the rate limit, and `serverStatus` reports the store under
`metrics.queryStats`.

An `aggregate` that explains is recorded too, its key naming the
verbosity: `queryPlanner` for `{explain: true}`, the `explain` command's
own (`allPlansExecution` by default). `$queryStats` with
`transformIdentifiers` lists every key with each field path, namespace and
user variable as its HMAC-SHA-256 under the given key, as mongod 8.2.11
prints it. A builtin variable and every literal stay as they were. All 257
cases of the live query-shape corpus match mongod.

#### Added

- `secantus-commands` (`query_stats.rs`): the store as mongod's
  `PartitionedCache` of `LRUKeyValue` maps, with its budget, eviction,
  sampling policy and `serverStatus` counters (`numEntries`, `numEvicted`,
  `numPartitions`, `queryStatsStoreSizeEstimateBytes`, ...). An entry is
  charged its key's BSON plus a per-command overhead measured from
  mongod's own estimate.
- `secantus-commands` (`params.rs`): `internalQueryStatsCacheSize`, a
  memory size string (2 for a value of another type, 6007012 for one that
  does not parse), and `internalQueryStatsSampleRate`, a number from 0 to 1.
- `secantus-commands` (`query_shape.rs`): identifiers serialized through
  `serializeIdentifier` / `serializeFieldPath` / `ExpressionFieldPath`'s
  rules, which `transformIdentifiers` turns into HMACs.

#### Fixed

- `$$CURRENT.a` is shaped as `$a`, and `$geoNear`'s `spherical` prints as
  `?bool` in a `$queryStats` key, as mongod does.

Fixes RM-532 except its remainder: entry sizes charged from measured
constants rather than C++ object sizes, mongod's hash-set order of
`otherNss`, and two unmeasured key components
(tests/backlog/test_rm_found_2026_11_10_rm215.py::test_query_stats_remainder,
tests/mongod_live/test_query_stats.py::test_identifiers_are_transformed_as_mongods,
tests/mongod_live/test_query_stats_store.py). Files RM-643 (a `$natural`
sort's key) and RM-644 (a hybrid search's `otherNss`).

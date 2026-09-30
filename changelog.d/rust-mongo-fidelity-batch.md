### The Rust MongoDB server stops losing rows through its indexes, and matches mongod on twenty more shapes

A sweep of every differential probe against mongod 8.2.11 found three ways an
index made the Rust server return fewer documents than it should. A sort over
a partial compound index, a sort or hint over a multikey index holding an empty
array, and an index range scan bounded by an array or a document each dropped
rows silently. All three are fixed, and the probe that should have caught them
now does.

The same sweep turned up a run of wrong answers and missing pieces: sorting
embedded documents by their encoded length instead of their value,
`$project: {_id: 1}` returning whole documents, `$slice` / `$indexOf*` /
`$range` argument handling, `maxTimeMS` not interrupting aggregations, and an
aggregate result over 16 MB sent as a message too large for any driver to
accept. Each was measured against mongod, fixed, and pinned by a probe that now
reports zero divergences.

#### Fixed

- **Silent data loss through indexes (Rust server).** `find({}).sort({a: 1, b: 1})`
  over a compound index partial on `a` returned only the rows the index holds;
  a sort or a hint over a multikey index dropped every document whose field is
  an empty array (`count({}, hint: ...)` too); and `{x: {$gt: [1, 2, 3]}}` with
  an index on `x` dropped `{x: [9]}`.
- **Sort order of embedded documents and nested arrays.** `find().sort()`
  compared raw-BSON sort keys, which lead with the value's length, so
  `{a: 2, b: [3]}` sorted above `{a: 5}`. Documents and arrays now compare by
  value, as the aggregation `$sort` always did.
- `$project: {_id: 1}` alone is an inclusion projection.
- `$slice` accepts a whole Decimal128, refuses a count outside int32 (28726 /
  28728) or not positive (28729), and counts a negative start from the right
  place; `$substrCP` and `$indexOfCP` / `$indexOfBytes` / `$indexOfArray`
  enforce int32 and accept whole decimals; `$range` is bounded by mongod's
  100 MiB memory estimate (146) instead of refusing past 100,000 elements.
- `maxTimeMS` interrupts aggregation stages, `distinct`, sorted `find` and index
  walks; read commands report an execution-time expiry under mongod's executor
  prefix, and `update` / `delete` fail the command rather than reporting a
  per-statement write error.
- An aggregate result over 16 MB answers `10334 BSONObjectTooLarge` instead of
  an 84 MB reply the driver rejects; decoded first batches respect the 16 MB
  budget.
- Regex `$` and `\Z` match before a final newline, as PCRE does; `\Z` was
  refused.
- `$jsonSchema` refuses `type: "integer"` (9) and unknown type names (2).
- `$bucketAuto` `POWERSOF2` answers an int for a whole power of two.
- A `writeConcernError` carries mongod's `errInfo.writeConcern` (`w`, `j`,
  `wtimeout`, `provenance`) and sits before `ok`; a write whose `w` names an
  unknown tag runs and then reports 79, instead of being refused.
- `drop` reports the collection's real index count as `nIndexesWas`.
- SASL errors match mongod (`Authentication failed.`, 334 for an unknown
  mechanism, 17 with no conversation), and `usersInfo {forAllDBs: true}` lists
  every user instead of none.
- **The streaming `hello` a driver's server monitor sends is held as mongod
  holds it.** A monitor that already had the current topology got its first
  streamed reply at once, one extra heartbeat per stream: the Go driver's
  `heartbeats_processed_more_frequently` test counted 12 messages against a
  limit of 10. A monitor with an out-of-date topology is now answered at once
  instead of held, returning to primary after `replSetStepDown` moves
  `topologyVersion` so the monitor hears about it immediately, and a
  malformed `topologyVersion` or `maxAwaitTimeMS` gets mongod's error instead
  of being accepted.
- A document that fails its validator gets mongod's full explanation in
  `errInfo.details`: every broken `$jsonSchema` rule in mongod's order, and
  every failing query clause, where it used to get `{operatorName: "$jsonSchema"}`.

#### Added

- **SCRAM-SHA-1 on the Rust server**, created for every user by default next to
  SCRAM-SHA-256 as mongod 8.2 does; `hello`'s `saslSupportedMechs` names the
  user's own mechanisms.
- **The localhost exception**: a fresh `--auth` Rust server lets a loopback
  connection create the first user, as mongod does. It refused every
  `createUser`, so it could never be given one.
- Decimal128 operands for `$pow`, `$atan2` and `$bucketAuto` `granularity`,
  correctly rounded and with mongod's special values and quanta.
- A multi-field filter rides a single-field index on one of its fields, and a
  sort under an unindexed filter walks the sort index -- the plans mongod picks.
- Probes: `max_time_expiry.py`, `int32_arguments.py`, `nested_value_sort.py`,
  `bucket_auto_granularity.py`, `validation_error_details.py`, `scram_auth.py`,
  `awaitable_hello.py`;
  `index_result_sets.py` now covers empty filters, compound sorts, hints,
  partial compound indexes and array / document bounds.

#### Changed

- Differential probes stop their embedded server before deleting its store; three
  of them ended in a `WT_PANIC` at exit.
- The driver gauges pass `--noop-heartbeat-seconds` and the other tuning flags
  through to the Rust server, which accepts them all.

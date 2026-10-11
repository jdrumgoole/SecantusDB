### Rust MongoDB server: time-series collections checked as mongod checks them, and `$out` keeps the target's validator

A time-series collection on the Rust server was a plain collection with a
label. Measured against mongod 8.2.11 over 187 scenarios, 139 answers
differed: any `timeseries` document was accepted, a measurement with no date
in its time field was stored, and updates were either let through or refused
with one message for every case. 25 now differ, nearly all of them because
mongod stores buckets in `system.buckets.<name>` and this server does not.

One bug found on the way affects every collection: `$out` dropped its target
and created it again bare, so the target's validator and indexes were gone
after the first `$out`, and the next one wrote documents the validator
would have refused.

#### Added
- `create` validates a `timeseries` definition (`timeField`, `metaField`,
  `granularity`, `bucketMaxSpanSeconds` / `bucketRoundingSeconds`), refuses
  it beside `capped`, `validator`, `viewOn`, `clusteredIndex` and
  `changeStreamPreAndPostImages`, and stores it with its defaults, as
  `listCollections` shows it on mongod.
- A time-series collection with a meta field is created with the
  `<meta>_1_<time>_1` index, and lists no `_id` index.
- `collMod` on a time-series collection: a wider `granularity` or bucket
  span, `expireAfterSeconds` (a number or `"off"`), and mongod's refusals
  for the rest.
- `$out` with `{db, coll, timeseries}` creates a time-series collection, or
  writes into an existing one with that definition.

#### Fixed
- `$out` keeps the target collection's options and indexes.
- An insert into a time-series collection refuses a document whose time
  field is missing or not a date (2).
- An update of a time-series collection follows mongod's rules, each with
  its own message (72): multi only, no upsert, no pipeline or replacement,
  a query on the meta field only, and changes to the meta field only.
- `findAndModify` on a time-series collection takes an unsorted `remove`
  and refuses everything else (72).
- Unique and text indexes are refused on a time-series collection, and a
  TTL index needs a partial filter on the meta field.
- A time-series collection cannot be renamed or renamed onto, watched by a
  change stream, read with `tailable`, `min` or `max`, or written to by
  `$merge`.
- `dropDatabase` answers `{ok: 1}` with no `dropped` field when the server
  runs as a replica-set member, as mongod does.
- `create` words an unknown field with its final full stop.

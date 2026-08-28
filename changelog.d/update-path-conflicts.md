### Overlapping update operator paths are rejected, as mongod does

An update whose operators target the same path — or where one path is a prefix
of another — was applied anyway. `{$set: {a: 2}, $inc: {a: 1}}` produced
`{a: 3}`; real mongod refuses it outright, because `$set` replaces the very
subtree `$inc` wants to walk into. We accepted 8 of the 12 overlapping shapes
mongod rejects, returning documents mongod would never produce, with no error to
notice.

Found by differential-probing the update operator family against a real mongod
rather than by any failing test.

#### Fixed

- An update whose operators touch equal or prefix-overlapping paths returns
  mongod's `ConflictingUpdateOperators` (code 40) with its exact message,
  `Updating the path 'X' would create a conflict at 'Y'`. Verified byte-identical
  across all six message shapes against mongod 8.3.4, on both servers.
- Sibling and disjoint paths are unaffected — `{$set: {"a.b": 2}, $inc: {"a.c": 1}}`
  still applies, as do sibling array indexes.
- The check splits on dots rather than comparing strings, so `ab` is not treated
  as overlapping `a`.
- A `$rename` claims both its source and destination against *other* operators,
  but its two endpoints are not compared with each other: mongod gives an
  overlapping pair its own error ("must not be on the same path", code 2), and a
  self-rename its own too ("must differ").

#### Also fixed

- The Rust server now attaches a failpoint's `errorLabels` alongside its
  `writeConcernError`. Without `RetryableWriteError` a driver never classifies
  the write as retryable and never retries, which left
  `/command_monitoring/unified/writeConcernError` failing on the Rust server even
  after the replay fix cleared it on the Python one.

### The Rust server rejects the specs it should, and owns up to Atlas-only commands

Three behaviours the Python server had and the Rust one didn't, found by
splitting the C and Ruby driver-conformance failures against the Python
server's own results so only the Rust-specific ones remained.

Unknown fields on `create` and on an index spec were silently accepted rather
than rejected. Real MongoDB fails them, and drivers rely on that: three
mongo-ruby-driver specs deliberately pass `invalid: true` and assert the
operation fails, which is how a typo in an index option gets caught at the
point it is made rather than becoming an index that quietly isn't what was
asked for. Both now answer with the same unknown-field error MongoDB gives.

The Atlas Search index commands — `createSearchIndexes`, `updateSearchIndex`,
`dropSearchIndex` — went unanswered entirely, so a client heard "no such
command" rather than "this needs Atlas". A non-Atlas MongoDB registers them and
fails them with a message naming Atlas, which is the difference between a
driver reporting a missing feature and reporting a broken server. Finally,
`$collStats` reported that a capped collection was capped but not what its
bounds were; the `max` and `maxSize` fields are now present.

#### Fixed

- `create` rejects unknown top-level options, and `createIndexes` rejects
  unknown fields on an index spec, with MongoDB's `Location40415`.
- `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` report
  `CommandNotSupported` naming Atlas, instead of `CommandNotFound`.
- `$collStats` reports `maxSize` and `max` for a capped collection alongside
  `capped`.

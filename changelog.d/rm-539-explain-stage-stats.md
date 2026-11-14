### Explain reports what the stages inside a `$facet`, `$unionWith` and `$lookup` did

At `executionStats`, the Rust MongoDB server now gives the stages inside a
`$facet` their own `nReturned` and memory figures, lists a `$unionWith` over
`$documents` as the `$queue` / `$project` / `$unwind` / `$replaceRoot` stages
mongod runs (each with its count), gives a `$lookup` whose pipeline makes its
own documents mongod's `totalDocsExamined`, `totalKeysExamined`,
`collectionScans` and `indexesUsed`, and reports the memory of the `$group` a
`$count` runs as. A `$sort` after `$documents` now estimates its bytes as
mongod does: each document is the storage its expression object built, not
its BSON.

#### Fixed

- `secantus-core` `docstore` / `aggregate`: an array an expression builds
  keeps each element's storage, which `$unwind` hands to the field it sets
  and `$replaceRoot` / `$replaceWith` make the new root, so a sort after
  `$documents` counts 245 bytes a document where it counted 117 (mongod
  8.2.11).
- `secantus-commands` explain: `$count` is run as its `$group` and
  `$project`, so the `$group` lists `maxAccumulatorMemoryUsageBytes` and
  `totalOutputDataSizeBytes`; the new `explain/sub_stages.rs` fills in the
  stages of a `$facet` (run classic, as mongod runs a facet's pipelines),
  of a `$unionWith` over `$documents` (also listed so, optimized, at
  `queryPlanner`), and a `$lookup` over `$documents`.

Fixes RM-539 (tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_each_stage_reports_what_mongod_does)

Filed while working it: RM-681 (a `$unionWith` over a collection is explained
as written), RM-682 (a `$lookup` over a collection reports nothing it
examined), RM-683 (a join-field `$lookup` is explained as a classic stage),
RM-684 (a 12-byte string is counted as held outside its `Value`), RM-685 (a
`$sort` and its `$limit` in a `$facet` are two stages) and RM-686 (a run of
`$documents` counts its documents as read BSON).

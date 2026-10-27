### Aggregation stages hold mongod's memory budgets, and `$group` spills to disk

The Rust MongoDB server now keeps mongod 8.2's per-stage memory accounting.
Each blocking stage is held to the same 100 MiB budget mongod gives it,
measured in mongod's own size estimates (a classic `Value`, an SBE value, a
document's fixed overhead), so the byte counts in its errors match mongod's.
A `$group` over its budget spills its groups to disk and merges them back
when `allowDiskUse` allows it (the default), and refuses when it does not. A
budget no spill can help -- one accumulator's array, one `$lookup` result,
one `$facet` document -- always refuses. Before this the server ran every
such pipeline in memory: 110 MB of documents through the RM-211 cases took
it to a 14.5 GB peak; it now refuses them as mongod does, under 1 GB.

#### Fixed

- `$group` with `allowDiskUse: false` over its budget is refused with 292,
  in the words of the engine mongod runs it in; with disk allowed it
  spills sorted partial states to a file per run and merges them, every
  accumulator's state folded as mongod folds it, the groups coming back in
  key order as mongod's do once spilled. An array past its budget from
  `$push`, `$addToSet`, `$concatArrays` or `$setUnion` is 146; a
  `$setWindowFields` partition cache past its budget with `allowDiskUse:
  false` is 5643011, counting only the fields the stage and the stages
  after it read, as mongod's dependency analysis keeps; a `$lookup` result
  past its budget is 4568 (146 when SBE runs the join); a `$facet`
  document is 4031700. Fixes RM-211
  (tests/backlog/test_rm_found_2026_10_20b.py::test_aggregation_memory_limits).
- `$setWindowFields` hands each output document to the per-document stages
  after it as it is made, and computes a window that does not move once
  per partition, instead of holding every output -- each with its own copy
  of the window's values -- and cloning them all again to emit them.
- A profiled aggregate that spilled reports `usedDisk: true` and its
  `$group`'s `groupSpills`, `groupSpilledBytes`, `groupSpilledRecords` and
  `groupSpilledDataStorageSize` (this server's own spills, which are not
  mongod's sampled ones).

#### Added

- The budgets are mongod's runtime parameters, with mongod's types, bounds
  and refusals: `internalDocumentSourceGroupMaxMemoryBytes`,
  `internalQueryMaxPushBytes`, `internalQueryMaxAddToSetBytes`,
  `internalQueryMaxConcatArraysBytes`, `internalQueryMaxSetUnionBytes`,
  `internalDocumentSourceSetWindowFieldsMaxMemoryBytes`,
  `internalLookupStageIntermediateDocumentMaxSizeBytes` and
  `internalQueryFacetMaxOutputDocSizeBytes`. As on mongod 8.2.11, an SBE
  `$group` is held to its own budget, which a client cannot change.
  `tests/mongod_live/test_aggregation_memory.py` lowers them on both
  servers to compare spills and refusals byte for byte.
- CI's mongod comparison no longer skips RM-211.

Not yet: a blocking `$sort`, `$bucketAuto`'s sort and a `find` sort have
no budget (RM-315).

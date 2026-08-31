### The wrong-typed-argument sweep, widened from codes to messages — 76 slots on the Rust server

The existing sweep compared error **codes** over 244 argument shapes, and both
servers read 244/244 clean. Comparing **messages** as well, over 685 shapes,
against mongod 8.2.11, found the Rust server diverging on **76 argument slots**.
Almost all of them were *silently accepted*: the wrong-typed value did not
merely return the wrong status, it made the server do something other than what
the caller asked and report success.

The probe's reach is exactly its case list, which is the recurring lesson here —
"244/244 clean" only ever meant "these 244 shapes are".

#### Fixed

- **Argument slots that took a wrong-typed value and ran anyway** — `hint`,
  `collation`, `readConcern`, `writeConcern`, `arrayFilters`, `ordered`,
  `bypassDocumentValidation`, `let`, `fields`, `sort`, `new`, `remove`,
  `filter`, `cursor` / `cursor.batchSize`, `validator`, `timeseries`,
  `capped` / `size` / `max`, `viewOn`, `changeStreamPreAndPostImages`,
  `partialFilterExpression`, `unique`, `sparse`, `expireAfterSeconds`,
  `dropTarget` and `tailable` / `awaitData` / `returnKey` / `showRecordId` /
  `allowDiskUse`, across `find`, `count`, `distinct`, `aggregate`, `insert`,
  `update`, `delete`, `findAndModify`, `create`, `collMod`, `createIndexes`,
  `dropIndexes`, `renameCollection`, `listCollections`, `listIndexes`,
  `getMore` and `killCursors`. Each now answers mongod's own code and wording.
- **Thirteen aggregation stages whose spec type was never checked** —
  `$addFields` / `$set` (40272), `$project` (15969), `$replaceRoot` /
  `$replaceWith` (40229), `$facet` (40169), `$bucket` (40201), `$sortByCount`
  (40147 / 40148 / 40149), `$geoNear` (10065), `$graphLookup`, `$unionWith`,
  `$setWindowFields`, `$densify`, `$fill` (9) and `$sample` (28745).
- **`aggregate` with a non-array `pipeline`** ran the whole collection through
  no stages and answered ok. Now `A pipeline must be an array of objects` (14).
- **An empty `updates` / `deletes` batch answered ok:1 with `n: 0`**, telling
  the caller a batch it never sent had been applied. Now InvalidLength, like
  `insert`.
- **`killCursors` with a non-array `cursors`** reported the named cursors
  killed while killing none.
- **A `hint` naming no index was ignored by `update` / `delete` /
  `findAndModify`**, so the write ran unhinted where mongod fails the command.
- **`$[identifier]` with no matching `arrayFilters` entry** was accepted when
  the target field was not an array — the engine's walk returns early for a
  non-array, so `{$set: {"a.$[e]": 1}}` against `{a: 1}` wrote nothing and
  reported success. mongod decides this from the update document alone.
- **`dropIndexes` by key pattern answered code 1 (InternalError)** — the crash
  code — for a shape mongod handles routinely. It now drops the matching index,
  or answers IndexNotFound (27).
- **`findAndModify: {remove: 1}`** was rejected: `remove` accepts a bool *or* a
  number, and a nonzero number is true.
- **`InvalidLength` is code 16, not 4** (4 is `NoSuchKey`) — on **both
  servers**. `insert` had carried the wrong code under a comment asserting that
  driver tests gate on "this exact code/codeName combo"; they gate on the
  codeName, and `bulkWrite` in the same codebase already answered 16.

#### Added

- `tools/probes/arg_types_messages.py` — the wide sweep, comparing `(code,
  errmsg)` rather than codes alone. It normalises the ORDER of mongod's
  expected-type lists, which differ between 8.2.1 and 8.2.11 (a patch bump) and
  so pin a build rather than a behaviour.
- `tests/test_rust_arg_types_sweep.py` (42 tests) and nine Rust unit tests,
  pinning the per-slot asymmetries: `count.limit` rejects an explicit null while
  `count.skip` beside it accepts one, `getMore.collection` reads null as absent
  (40414) rather than wrong-typed, `createIndexes`' `unique` accepts `1.5` and
  quotes the spec back with mongod's own unclosed quote, and `$densify`
  capitalises "The" where `$setWindowFields` does not.

### Wrong-typed command arguments on the Python server: the message sweep

The wrong-type sweep that closed the Rust server compared error CODES only.
Comparing MESSAGES over the same 685 shapes against mongod 8.2.11 found the
Python server divergent on 409 of them, including **18 that answered
`internal server error`** — an `int()` or an iteration over a value whose type
was never checked. This closes 311 of the code divergences and 31 of the
message-only ones; `createIndexes` and `collMod` are the measured remainder.

#### Fixed

- **A wrong-typed `count.limit`, `count.skip`, `listCollections.cursor.batchSize`
  or `update.updates.arrayFilters` crashed the command handler** and answered
  `1 internal server error`. All 18 crashing shapes now answer mongod's code.
  `count`'s two slots are deliberately not symmetric: `limit` answers
  `2 limit value is not a valid number` (and `9 Expected an integer` for a
  non-integral double), `skip` answers `14` with the IDL expected-types list —
  which is what mongod does.
- **A negative `count` limit was ignored** where mongod counts its absolute
  value, so `limit: -3` returned the whole collection instead of 3.
- **Python class names leaked into error messages** — `'ObjectId'` for
  `objectId`, `'datetime'` for `date`, `'bytes'` for `binData`. Three partial
  copies of the type-name mapper have been collapsed into `secantus.bsontypes`,
  probed against mongod and matching the Rust engine's existing vocabulary. This
  corrects the type name in roughly 90 message sites across every engine.
- **Wrong-typed `hint` answered `2 invalid hint type: int`** instead of mongod's
  `9 Hint must be a string or an object`, on `find` / `count` / `aggregate` /
  `update` / `delete`. On the batch commands it is a command-level error, so
  nothing is written.
- **Wrong-typed document options were silently accepted** — `collation`,
  `readConcern`, `validator`, `timeseries`, `filter` and `cursor` across
  `find` / `aggregate` / `update` / `delete` / `findAndModify` / `create` /
  `distinct` / `listCollections` now answer `14` under mongod's own IDL path.
- **Aggregation stages reported a generic `14` for a non-document spec.** Each
  now answers mongod's own code and wording: `$project` 15969, `$addFields`
  40272, `$replaceRoot` 40229, `$bucket` 40201, `$bucketAuto` 40240, `$sample`
  28745, `$geoNear` 10065, `$redact` 17053 (a runtime executor error, with the
  wrapper), and `$densify` / `$fill` / `$setWindowFields` / `$unionWith` 9.
- **Boolean options were ignored or misreported** — `find`'s `tailable`,
  `awaitData`, `returnKey`, `showRecordId` and `allowDiskUse`,
  `aggregate.allowDiskUse`, `ordered` on all three write commands, and
  `renameCollection.dropTarget`. A string `tailable` used to reach the cursor
  machinery and answer "tailable cursor requested on non capped collection".

### A `collMod` of an index makes mongod's `modify` event

The `modify` change event of a `collMod` that changes an index now names
the index and what changed, as mongod 8.2.11 does, and so carries the same
resume token.

#### Fixed

- The oplog entry of `collMod {index: ...}` records `index: {name, <changed
  options>}` in `o` and the options as they were in `o2.indexOptions_old`
  (`{}` for a conversion to unique; no `index` when nothing changed). The
  event's `operationDescription` is `{index: {...}}` and its
  `stateBeforeChange` adds `indexOptions`, for `hidden`,
  `expireAfterSeconds`, `prepareUnique` and `unique` alike.

Fixes RM-733 (tests/backlog/test_rm_found_2026_11_21_resume_tokens.py::test_a_collmod_of_an_index_is_a_modify_event_with_the_index)

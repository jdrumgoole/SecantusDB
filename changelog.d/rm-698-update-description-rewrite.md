### A change stream's `$match` on `updateDescription` or `fullDocumentBeforeChange` joins the oplog read

The Rust MongoDB server now rewrites a `$match` after `$changeStream` on
`updateDescription` and `fullDocumentBeforeChange` into the oplog filter as
mongod 8.2.11 does, so the read and its `explain` match mongod's.

#### Fixed

- A field of `updateDescription.updatedFields` is read as that field of the
  update entry's `$set` or of its diff's `i` / `u`, a name in `removedFields`
  as a field of `$unset` or of the diff's `d`, and other `updateDescription`
  predicates as "an update"; a predicate on `fullDocumentBeforeChange` reads
  updates and deletes (`op` in `d`, `u`). An `$expr` on either goes the same way.

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_the_update_or_pre_image_joins_the_oplog_filter)

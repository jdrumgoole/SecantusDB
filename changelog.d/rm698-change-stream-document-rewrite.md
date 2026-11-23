### A `$match` on `documentKey` or `fullDocument` after a change stream joins the oplog read

The Rust MongoDB server now rewrites a `$match` on `documentKey` and
(inexactly, as mongod does) on `fullDocument` that follows a `$changeStream`
into predicates on the oplog entry, so its `explain` shows mongod 8.2's
read filter, unwind filter and `queryHash`.

#### Fixed

- `documentKey` predicates become `o2` of an update or insert and `o` of a
  delete; `fullDocument` predicates become `o` of an insert or replacement,
  with every other update kept. Part of RM-698; `ns`, `to`,
  `updateDescription`, `fullDocumentBeforeChange`, the renamed fields and
  `$expr` remain open.

Fixes part of RM-698 (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_the_document_joins_the_oplog_filter)

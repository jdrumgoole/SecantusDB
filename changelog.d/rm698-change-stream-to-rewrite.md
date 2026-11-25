### A `$match` on `to` after a change stream joins the oplog read

The Rust MongoDB server now rewrites an `$eq` or `$in` on a change event's
`to`, `to.db` or `to.coll` into a predicate on a rename's `o.to` in the
oplog read, as mongod 8.2.11's `matchRewriteGenericNamespace` does, so the
read's filter and `queryHash` match mongod's.

#### Fixed

- A `$match` on `to` after `$changeStream` left the oplog read unfiltered
  (`queryHash` 90A52575 where mongod's `{"to.coll": "c"}` is A491FA53).

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_the_rename_target_joins_the_oplog_filter)

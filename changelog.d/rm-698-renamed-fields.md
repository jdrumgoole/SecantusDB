### A change stream's `$match` on `clusterTime`, `lsid` or `txnNumber` joins the oplog read

The Rust MongoDB server now renames a `$match` after a `$changeStream` on
`clusterTime` (the entry's `ts`), `lsid` or `txnNumber` into the oplog
filter, as mongod 8.2.11 does, so the stream's `explain` and plan-cache
`queryHash` match mongod's.

#### Fixed

- A predicate on `clusterTime`, `lsid` or `txnNumber` joins the read's
  events and, as mongod's transaction filter does, the entries that hold a
  transaction's events; the unwind's filter leaves them out, as mongod's
  does. A `$type` or an `$expr` on those fields is still not rewritten
  (RM-698 stays open for them, `updateDescription` and
  `fullDocumentBeforeChange`).

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_a_renamed_field_joins_the_oplog_filter)

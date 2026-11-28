### A change stream's `$type`, `$regex` or `$size` joins the oplog read

The Rust MongoDB server now clones a `$type`, `$regex` or `$size` in a
`$match` after a `$changeStream` onto every oplog field the path renames
to, as mongod 8.2.11 does, instead of leaving it out of the read.

#### Fixed

- A `$type` (printed as its sorted type numbers, after the `number`
  alias), a `$regex` (with its `$options`), a `$size`, and an `$in` holding
  a regex on `updateDescription.updatedFields`, `documentKey`,
  `fullDocument`, `clusterTime`, `lsid` or `txnNumber` are now part of the
  oplog filter and the unwind's, with mongod's `queryHash`.

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_type_regex_or_size_on_a_renamed_path_joins_the_oplog_filter)

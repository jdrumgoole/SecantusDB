### A time-series read through the meta index counts the keys and buckets it examined

A read of a time-series collection filtered on its meta field goes through
mongod's bounded scan of the default index over the buckets, and its
slow-query line counts what that scan examined, as mongod 8.2.11's does
(three keys and three buckets for `m >= 2` over five series). A `$group`
after such a `$match` streams over that scan, where it read the collection
whole and counted nothing.

#### Fixed

- `keysExamined` and `docsExamined` for a time-series `find` or `aggregate`
  filtered on the meta field: the index over the buckets is read within
  the plan's bounds, and its keys are counted.
- A time-series `$match` on the meta field followed by a `$group` reads the
  buckets through that index.
  Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_group_after_a_meta_match_counts_what_it_examined)

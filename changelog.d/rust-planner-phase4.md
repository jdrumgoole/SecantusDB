### The Rust MongoDB server plans queries as mongod does

Where several indexes could serve a query, the Rust MongoDB server now runs
mongod's trial of the candidate plans instead of a fixed order of
preference: works counted round-robin, mongod's plan ranker's scores,
AND_SORTED intersections, one plan per predicate on an index's array field.
The winner decides both what `explain` reports, with mongod's
`rejectedPlans`, and the order documents come back in. Scans a sort merges
come out through the same priority queue mongod uses, and a `$match` moves
ahead of an `$addFields` or `$unwind` it does not depend on. The storage
router reads an index for `$ne`, `$nin`, `$not`, `$exists`, `$type`, `$mod`,
regexes and `$elemMatch`, where it used to scan the collection.
`tools/probes/find_order.py` differs from mongod 8.2.11 on 77 of 6,531
replies (97 before) and `tools/probes/explain_commands.py` on 35 of 791
(72 before).

#### Fixed

- Equal documents from the scans an `$in` sort merges came out scan by
  scan; they now come out in mongod's priority-queue order. Fixes RM-050
  (`tests/backlog/test_rm_029_056.py::test_ties_between_merged_scans`).
- With several indexes on a query's fields, the one read was a fixed
  preference rather than mongod's trial winner. Fixes RM-052
  (`tests/backlog/test_rm_029_056.py::test_which_predicate_index_is_read`).
- A `$match` after an `$addFields` that writes none of its fields did not
  read its index. Fixes RM-053
  (`tests/backlog/test_rm_029_056.py::test_match_after_add_fields_reads_its_index`).
- A regex under a collation with two indexes on the field, and `$in` sorts
  after writes, read in a different order. Fixes RM-054
  (`tests/backlog/test_rm_029_056.py::test_regex_under_a_collation_with_two_indexes`,
  `tests/backlog/test_rm_029_056.py::test_in_sort_after_writes`).
- `explain` reported no rejected plans. Fixes RM-070
  (`tests/backlog/test_rm_057_084.py::test_explain_reports_rejected_plans`).
- `explain` of an `aggregate` lists the cursor's dependency projection,
  `$group` served by a DISTINCT_SCAN, stages as mongod parses them, and
  per-stage counts under a `$limit`, as mongod does.
- The storage router read the whole collection for predicates mongod
  answers from an index (formerly RM-069): `{a: {$ne: 5}}` over `a_1`
  takes 0.7 times as long as before, and a regex prefix a hundredth.
- `update_one`, `delete_one` and `findAndModify` over one plain index no
  longer read the matching documents twice.

#### Changed

- A query two indexes could serve costs about 5 times as much on 2,000
  documents, for the trial (RM-056). mongod's plan cache, which runs the
  trial once per query shape, is not reproduced (RM-154).

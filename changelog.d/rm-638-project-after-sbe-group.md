### A `$project` behind a pushed-down `$group` is sized as built

A `$sort` behind `[{$group: ...}, {$project: ...}]` now counts the documents
the projection builds over the groups SBE hands back, as mongod 8.2.11 does
(2450 bytes over ten groups, not the 1170 their BSON encodes).

#### Fixed

- The storage SBE's `$group` builds its output in is kept for the modelled
  stages behind it, so `totalDataSizeSortedBytesEstimate` and the memory
  budget match mongod.

Fixes RM-638 (tests/backlog/test_rm_found_rm519.py::test_a_project_after_a_pushed_down_group_builds_its_documents)

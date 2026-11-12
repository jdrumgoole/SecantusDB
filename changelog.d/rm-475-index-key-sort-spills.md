### Rust MongoDB server: a SORT of index keys past its budget spills to disk

A `find` sorted over index keys (the `SORT` below a `FETCH`, `type:
"default"`) with `allowDiskUse` now spills past its memory budget as mongod
8.2 does: it writes its working-set members to sorted runs in mongod's
`WorkingSetMember` format and merges them, and the profile entry, slow-query
line and `execStats` report `usedDisk`, `sortSpills`, `spilledRecords` and
`spilledBytes` as mongod's do (2000 members under a 100000-byte budget: two
spills of all 2000, 130000 bytes). It reported no spill before.

#### Fixed

- A SORT of index keys past its budget spills, with mongod's counts. Fixes
  RM-475 (tests/backlog/test_rm_found_rm375.py::test_a_sort_of_index_keys_past_its_budget_spills).
  Filed RM-555 (a collation's keys in the spill) and RM-556 (the snapshot
  id's effect on the compressed size, a decision), and RM-558 (a
  mongod-live test whose reference answer races a 5 ms lock wait).

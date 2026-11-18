### An SBE sort counts the slot of a field a document lacks

The Rust MongoDB server's SBE sort (one a `$group` is pushed down behind)
now counts every slot of its row in the memory it holds, as mongod 8.2
does: a field a document lacks is `Nothing`, 9 bytes. `sortTotalDataSizeBytes`
and the point the sort spills now match mongod's.

#### Fixed

- Fixes RM-616 (tests/backlog/test_rm_found_2026_11_13_rm506.py::test_a_missing_field_counts_its_slot)

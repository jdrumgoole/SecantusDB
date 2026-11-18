### A profile entry counts the operation's yields

The Rust MongoDB server's `system.profile` entries wrote `numYield: 0` for
every command; they now carry the yields the operation made, as its
slow-query log line already did. A batched deleteMany also splits a batch
once it has run 5 ms (`batchedDeletesTargetBatchTimeMS`) and may yield
before finishing it, as mongod 8.2 does.

#### Fixed

- Profile entries count `numYield`; a batched delete splits on its time target.
  Fixes RM-609 (tests/backlog/test_rm_found_rm501.py::test_a_delete_many_profile_entry_counts_its_yields)

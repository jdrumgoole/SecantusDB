### A `collMod` of a missing collection reports mongod's locks

The Rust MongoDB server's slow-query line for a `collMod` of a collection
that does not exist now counts the locks mongod 8.2.11 counts: a global IS
alone, one write round holding the collection in X twice, then the
entry point's read of the latest optime (transition `w: 2`, global
`r: 2, w: 1`, collection `W: 2`). It took the plain write chain once.

#### Fixed

- Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control[collMod of one not there])

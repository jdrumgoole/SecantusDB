### Startup writes mongod's `Dropping spill idents` line

The Rust MongoDB server now writes `Dropping spill idents` (10682200) on
`initandlisten` right after `Initializing all collections in durable
catalog`, as mongod 8.2.11 does, with `numIdents` counting the spill tables
an earlier run left and the open dropped.

#### Fixed

- Startup log: `Dropping spill idents` (10682200) with its `numIdents`.
  Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_drops_spill_idents_after_the_durable_catalog)

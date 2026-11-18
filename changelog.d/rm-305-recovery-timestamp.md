### The Rust MongoDB server logs WiredTiger's recovery timestamp at startup

Right after `WiredTiger opened`, mongod 8.2 writes `WiredTiger
recoveryTimestamp` (23987) with the timestamp WiredTiger recovered to. The
Rust MongoDB server now reads it from WiredTiger (`get=recovery`) and writes
the same line.

#### Fixed

- Startup log: `WiredTiger recoveryTimestamp` (23987, `RECOVERY`) follows
  `WiredTiger opened`, as on mongod 8.2.11. RM-305 stays open for the
  startup lines still missing.

Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_writes_the_recovery_timestamp_after_opening)

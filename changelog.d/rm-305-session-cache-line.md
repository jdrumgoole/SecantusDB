### The Rust MongoDB server logs its session cache's size at startup

After `WiredTiger recoveryTimestamp`, `secantusd-mdb` now writes `WiredTiger session cache max value has been set` (9086700) on `initandlisten`, as mongod 8.2.11 does, with `sessionCacheMax` half the `session_max` WiredTiger was opened with.

#### Fixed

- Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_writes_the_session_cache_line_after_recovery)

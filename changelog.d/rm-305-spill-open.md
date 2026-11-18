### The Rust MongoDB server opens its spill engine at startup, as mongod does

mongod 8.2 opens its spill WiredTiger (`<dbpath>/_tmp/spilldb`) as it
starts and logs the open on `initandlisten`. `secantusd-mdb` opened it only
at the first spill, and its startup log went straight from the session
cache line to `Waiting for connections`.

#### Fixed

- `secantusd-mdb` opens the spill engine at startup, with mongod's
  message configuration, and logs `Opening spill WiredTiger` (10158000),
  the engine's own `WiredTiger message` lines (22430) and `Spill WiredTiger
  opened` (10158001) after `WiredTiger session cache max value has been
  set`, as mongod 8.2.11 does. The open's time counts in
  `createStorageEngineMillis`.

Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_opens_the_spill_wiredtiger_after_the_session_cache)

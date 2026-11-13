### The server keeps mongod's lock file, and its log says how the store opened and how long the start took

The Rust MongoDB server now keeps `mongod.lock` in its data directory as
mongod 8.2 does. The file holds the server's process id while it runs, and
a clean shutdown empties it. A server that finds the file still holding a
process id knows the last run did not shut down cleanly, and says so. Its
start then ends the way mongod's does: `Opening WiredTiger` with the
configuration the store was opened with, `WiredTiger opened` with how long
that took, and `mongod startup complete`, which reports whether the start
followed a clean shutdown and how long each step of the start took.

#### Added

- `mongod.lock` in the data directory (`secantusd-rs`, the embedded
  `secantus_mdb::Server` and the test launcher). It is taken with an
  exclusive lock before WiredTiger opens and holds the process id. It is
  emptied only after the store closes cleanly. A second server on the same
  directory is refused with mongod's `Unable to lock the lock file: ...
  Another mongod instance is already running on the ... directory`. The
  PostgreSQL server takes no lock file.

#### Fixed

- Startup writes mongod's `Detected unclean shutdown - Lock file is not
  empty` (STORAGE 22271, a warning naming `lockFile`) when the lock file
  still held a process id. It then writes `Opening WiredTiger` (22315,
  `config`) and `WiredTiger opened` (4795906, `durationMillis`) after the
  options, and ends on `initandlisten` with `mongod startup complete`
  (CONTROL 8423403). That line carries `Startup from clean shutdown?` and
  mongod 8.2.11's statistics, in its keys and order for a new or an
  existing store and for a standalone or a replica-set member. A step this
  server takes is timed: binding, the lock file, the store's open, the FCV,
  the cluster parameters, the users, the replica set's collections, the
  startup log, the accept loop and the total. A step it does not take
  reads 0, as mongod reports a step under a millisecond.
  Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_startup_writes_the_storage_and_startup_complete_lines,
  tests/backlog/test_rm_001_028.py::test_startup_reads_an_unclean_shutdown_from_the_lock_file).
  The rest of mongod's startup lines, starting with WiredTiger's own
  messages, stay open under RM-305
  (tests/backlog/test_rm_001_028.py::test_startup_writes_wiredtigers_own_messages).

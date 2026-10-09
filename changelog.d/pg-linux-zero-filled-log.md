### The Rust PostgreSQL server zero-fills its log on Linux

On Linux every durable commit ends in an `fdatasync` of the log, and
WiredTiger's log file is reserved on disk but never written, so each of those
syncs also had the filesystem record newly written blocks. The server now
writes zeros through a new log file when it creates it, as PostgreSQL does for
a WAL segment.

#### Changed

- `secantusd-pg` on Linux, when commits sync (the default; not under
  `SECANTUS_TEST_FAST_STORAGE=1`), creates its log files zero-filled and
  16 MB each where they were 128 MB. A store written by an earlier version
  opens unchanged, and an earlier version opens a store written by this one.
- macOS and the MongoDB server are unchanged.

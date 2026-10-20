### Rust MongoDB server: `fsync` with `lock: true`

#### Added

- `fsync: 1, lock: true` takes mongod's lock against writes. A background
  owner holds the global lock in S, so writes wait while reads go on. Locks
  nest (`lockCount`), and the writes resume with the last `fsyncUnlock`. The
  replies match mongod 8.2.11 field for field, including `fsyncUnlock`'s
  count and its refusal (20) when nothing is locked. Backup tools use the
  pair. It used to be refused with 9.

Fixes RM-220 (`tests/backlog/test_rm_found_2026_10_20b.py::test_fsync_lock`).

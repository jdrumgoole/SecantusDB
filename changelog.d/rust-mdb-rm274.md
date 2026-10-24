### Tests: the RM-185 lock counts and the RM-219 `dbCheck` comparison no longer move with the machine's load

The RM-185 lock-count tests compare `serverStatus.locks` deltas with mongod
8.2.11's. On a loaded machine the backlog verifier reported them REGRESSED in
about half of its full runs (RM-274). Under load, `$out` replacing its target
read 8 global IS instead of 7, and `$merge` creating its target read 4
instead of 2, in all five reference runs.

The cause is mongod's slow-operation log. mongod takes the global lock in IS
for each operation it logs as slower than `slowms` (100 ms), to read the
storage statistics for the `Slow query` line. Under disk load, creating a
collection crosses 100 ms. The investigation, on 2026-10-24:

- Twelve reference mongods at once reproduced it in nearly every run.
- One mongod beside fsync-heavy disk load reproduced it.
- CPU load, and eleven idle mongods, did not.
- With `slowms` out of reach, 120 of 120 runs read 7 and 2.
- With `slowms` 0 on an idle mongod, every run read the extra locks.

#### Fixed

- `tests/backlog/test_rm_185_locks.py`: a module fixture sends both servers
  the same `profile` command, setting `slowms` out of reach, and restores it
  afterwards.
  - Twelve concurrent runs of the module, which failed 12 of 12 times
    before (three or four tests each), now pass 12 of 12.
- Fixes RM-274. Its `Verify` was `none`; the regression tests are the
  module's five `backlog_fixed("RM-185")` tests.
- `tests/backlog/test_rm_219_db_check.py`, a second flake found the same
  way. It failed in 4 of 24 runs with twelve at once, for two reasons:
  - The test collected every oplog command entry in its window. A new
    primary creates `config.sampledQueriesDiff` and its TTL index in the
    background, and under load those landed in the window. Only `dbCheck`'s
    own entries are compared now.
  - mongod writes `local.system.healthlog` through a deferred writer, after
    the oplog, so the test sometimes read mongod's health log as empty. It
    now waits for the log's `dbCheckStop` record as well.
  - With both changes, 36 of 36 runs pass with twelve at once.

#### Filed

- RM-299: the lock is a divergence of its own. The Rust server writes the
  `Slow query` line but counts no global lock for it, and logs no line for a
  `$merge`'s writes
  (`tests/backlog/test_rm_185_locks.py::test_a_slow_operation_takes_the_global_lock_to_log`).

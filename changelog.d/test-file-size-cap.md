### The test suite caps any one file at 4 GiB, and reaps abandoned reference mongods

On 2026-11-06 the disk of the machine that runs the suite filled: 427 GB of
its 926 GB was one `mongod.log`, from a reference mongod that outlived its
test on 2026-10-26 and went on logging for six hours. Every build and test on
the machine then failed with "No space left on device".

#### Changed

- `tests/conftest.py` lowers the soft `RLIMIT_FSIZE` to 4 GiB in every pytest
  process, and every process the suite starts inherits it. A server that
  writes past it is killed by SIGXFSZ, so a runaway log becomes a dead
  server and a failing test rather than a full disk; inside pytest the write
  fails with EFBIG. `SECANTUS_TEST_MAX_FILE_BYTES` changes the cap, and `0`
  turns it off. Not on Windows.
- `tests/mongod_reference.py` names a reference mongod's dbpath
  `secantus-probe-<pid>-mongod-*`, so the session-start sweep reaps it when
  the test process that made it is gone.
- That sweep (`python_tasks._sweep_stale_probe_tmp`) also reaps the old
  `differential-mongod-*` dbpaths, and keeps any dbpath whose `mongod.lock`
  names a running process: a mongod that only appends to its log leaves the
  directory's mtime old.

#### Tests

- `tests/test_file_size_cap.py`: the cap is in force, a child inherits it, and
  a non-Python child writing past a lowered limit dies of SIGXFSZ.
- `tests/test_clean_pytest_tmp.py`: the sweep's mongod cases, and the
  reference mongod's prefix pinned to the sweep's.

#### Fixed

- `tests/backlog/test_rp_named_parse_cancel.py` (RP-303's regression test)
  failed this PR's CI on a trial its cancel never met: the Parse answered
  0.007 s after the cancel left, sooner than the cancel's own connection
  reaches the server. A trial whose cancel left under 0.1 s before an
  answered Parse is now aimed again, like one that left after it, and the
  "cut short" check allows the same margin. A cancel that leaves in time and
  is lost still fails.

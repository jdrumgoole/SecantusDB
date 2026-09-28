### Two test harnesses stopped hiding what the servers were telling them

Both changes are to harnesses rather than to a server, and both had the same
shape: a real signal from a database was being discarded before anyone could
read it.

#### Fixed

- **A crashed oracle is now an error, not silence.** The differential gate
  spawns a real `mongod` and compares against it, but sent that server's output
  to `DEVNULL` and discarded its exit status — so a `mongod` that died
  mid-module was indistinguishable from one the harness stopped on purpose,
  while every comparison made after it died was against nothing at all and the
  gate went on reporting agreement. This is not hypothetical: a `mongod` on the
  Windows dev box really did abort on 2026-09-22, and the only surviving
  evidence was a minidump with no log beside it (its exception stream reads
  `0xE0000001`, `EXCEPTION_NONCONTINUABLE` — `mongod`'s own fatal-assertion
  path, not a memory fault — but the assertion message was gone for good). The
  gate now captures the oracle's output to `<dbpath>/mongod.log`, and an
  unexpected exit preserves that log outside the doomed dbpath and raises with
  its tail attached.

- **The Rust PG server was being killed, not stopped, on Windows.** The slice
  tests' `_Server.__exit__` called `proc.terminate()` — SIGTERM on POSIX, but
  `TerminateProcess` on Windows, an immediate kill that runs no handler, so
  WiredTiger never closed and anything not yet checkpointed was gone. Seven
  tests failed as a result, including the cross-language hand-off cases where
  the Python server opened the store and found it empty. The binary already
  installed a Windows console control handler (the `ctrlc` crate with
  `termination`); it was simply never sent a signal it could catch. It is now
  spawned with `CREATE_NEW_PROCESS_GROUP` and stopped with `CTRL_BREAK_EVENT`,
  the pattern the Rust binary smoke tests already used. On the Windows dev box
  that moves the file from 10 failures to 3; the remaining three are a
  different root cause (a FATAL racing the socket close) and are recorded in
  `tasks/backlog.md`.

#### Added

- Twelve `$geoWithin` / `$centerSphere` cases in the differential gate, run both
  over a `2d` index and without one. `$centerSphere`'s radius is in radians, so
  anything at or past pi covers the whole sphere and must match every document —
  which is what the mongo-java-driver's own fixture asserts with r=4. That test
  failed once in the 2026-09-21 gauge and passed again on 2026-09-27; it was a
  flake, but nothing pinned the behaviour, so a real regression would have
  looked identical to one. Probed against 8.2.11: 0 of 12 divergent.

#### Changed

- The gate's `mongod` is spawned with `CREATE_NO_WINDOW` on Windows. A console
  executable with no creation flags allocates its own console, so under
  `-n auto` every xdist worker popped a terminal window on the desktop.

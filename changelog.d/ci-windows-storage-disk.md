### The Windows storage-engine job filled a small drive while a 147 GB one sat idle

`storage-engine (windows-latest)` failed with `No space left on device` and
`WinError 112` on a change that added no storage at all — the second Windows
disk exhaustion on this job.

The suite genuinely needs the room. `tests/conftest.py` **refuses** a
`tmp_path_retention_policy` that deletes a passed test's directory mid-session,
because one was tried and raced WiredTiger's background threads into a
`WT_PANIC` cascade, so every test's WiredTiger home stays on disk for the whole
session by design. Both engines already ship `prealloc=false`, the lever that
tripwire names, and Rust's larger `file_max` is a documented production-throughput
choice — none of that is the problem.

**The problem was which drive it needed the room on.** Python's `tempfile` reads
`TMP`/`TEMP`, which on this image default to
`C:\Users\runneradmin\AppData\Local\Temp`. `RUNNER_TEMP` is `D:\a\_temp`,
and D: has ~147 GB free. The job filled C: while D: sat at 3 GB used of 150.

That was found by adding the reporting first — and the first version of that
reporting was itself wrong, running `df -h .`, which measures the *workspace*
drive (D: on Windows) and would cheerfully have shown 147 GB free on the run
that died of a full C:. It now reports the directory the tests actually write
to.

#### Fixed

- `.github/workflows/test.yml`, `storage-engine` job: `TMP` / `TEMP` / `TMPDIR`
  point at `RUNNER_TEMP` on Windows, so the per-test WiredTiger homes land on
  the drive with the space.

#### Changed

- Reclaim the Android SDK on Windows, mirroring the reclaim the `test` job
  already has on Linux (MSVC / CMake / Python / LLVM all live elsewhere).
- Report free space for the **test temp directory** before and after the storage
  tests, so the delta sizes the suite's real appetite. The "after" step is
  `if: always()`, because a report that runs only on success never prints on the
  run that needs it.

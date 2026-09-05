### The Windows storage-engine job ran out of disk, and nothing recorded how close it was

`storage-engine (windows-latest)` failed with `No space left on device` and
`WinError 112` on a change that added no storage at all. It is the second
Windows disk exhaustion on this job.

The obvious fix is not available: `tests/conftest.py` **refuses** a
`tmp_path_retention_policy` that deletes a passed test's directory mid-session,
because one was tried and raced WiredTiger's background threads into a
`WT_PANIC` cascade. Every test's WiredTiger home therefore stays on disk for the
whole session by design, and the concurrency suite at the end of this job is the
heaviest writer of them.

Both engines already ship the lever that tripwire names (`prealloc=false`), and
Rust's larger `file_max` is deliberate and documented for production throughput,
so neither is something to quietly change.

What was missing was **any measurement**. The `test` job prints `df -h` around
its Linux reclaim; this job printed nothing, so the failure could not be sized
after the fact — there is no record of how much space there was or how much the
run used.

This adds the mitigation Linux already has, and the instrumentation to size a
real fix if the mitigation is not enough. It does **not** reduce what the suite
consumes; the delta the next run prints is what would tell us whether that is
needed.

#### Changed

- `.github/workflows/test.yml`, `storage-engine` job: reclaim the Android SDK on
  Windows (MSVC / CMake / Python / LLVM all live elsewhere), and report free
  space before and after the storage tests. The "after" report is `if: always()`,
  because a report that only runs on success never prints on the run that needs
  it.

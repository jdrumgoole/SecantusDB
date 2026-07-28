### Flush the PGO profile explicitly so the arm64-macOS binary build completes

With mimalloc no longer instrumented, the arm64-macOS binary's PGO instrumented
stage ran the profiling workload cleanly but wrote no `.profraw`: the LLVM
profiling runtime's atexit flush doesn't fire under the release workflow's
SIGTERM shutdown on that runner (Linux flushes normally), so the profile-merge
step had nothing to merge. The instrumented stage-1 build now compiles an
explicit `__llvm_profile_write_file()` into the shutdown path (behind a
`pgo-instrument` cargo feature, off for every normal build), flushing the profile
deterministically before exit.

#### Fixed

- `secantusdb` binary: the arm64-macOS two-stage PGO build now writes its profile
  under the workflow's SIGTERM shutdown (explicit `__llvm_profile_write_file()`
  behind the instrumented-only `pgo-instrument` feature), so the profile-merge
  and optimized stages complete.

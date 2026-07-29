### Restore full PGO on the arm64-macOS standalone binary

The arm64-macOS `secantusd-rs` binary's on-target profile-guided optimization is
working again, after a run of macos-14-runner-specific failures. Two quirks (both
Linux-immune) are now handled: the PGO instrumented build drops mimalloc (whose
instrumented allocator internals segfault at startup), and — the crux — the
instrumented binary writes its own profile on shutdown, because the profiling
runtime on that runner never wires up its `LLVM_PROFILE_FILE` at-exit write (a
clean `--version` exit produced no `.profraw` at all). A CI diagnostic pinned
that down; the binary now calls `__llvm_profile_set_filename` +
`__llvm_profile_write_file` to a known path (behind the instrumented-only
`pgo-instrument` feature), yielding a valid, mergeable profile (23.9k functions).
Both binary targets ship with full two-stage PGO again.

#### Fixed

- `secantusdb` arm64-macOS binary: full on-target PGO restored — the instrumented
  stage self-writes its profile (the runtime's env-driven at-exit write is inert
  on the macos-14 runner) and drops mimalloc to avoid an instrumentation segfault.

### Fix the arm64-macOS binary's PGO build crashing on mimalloc instrumentation

The standalone `secantusd-rs` binary's two-stage profile-guided-optimization
build segfaulted at startup on the arm64-macOS CI runner: the PGO **instrumented**
stage compiles mimalloc's own allocator internals with profiling counters, and
`__llvm_profile_instrument_target` faults (EXC_BAD_ACCESS) when it runs *inside*
mimalloc's first page allocation (from `LogBuffer::new` in the server bind path),
re-entering the half-initialized global allocator. The instrumented stage now
builds with the system allocator (`--no-default-features`, gating mimalloc behind
a default-on `mimalloc` cargo feature); the optimized final binary still ships
mimalloc, and the collected profile — a hint whose unmatched functions are
ignored — is unaffected by the allocator swap. The PyPI wheel's embedded server
was never affected (it consumes a committed profile, not on-target instrumentation).

#### Fixed

- `secantusdb` binary: the arm64-macOS two-stage PGO release build no longer
  segfaults in the instrumented stage. mimalloc is now a default-on cargo feature
  so the instrumented build can opt out (`--no-default-features`) while the
  shipped binary keeps mimalloc.

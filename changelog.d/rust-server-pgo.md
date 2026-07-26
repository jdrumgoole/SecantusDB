### Rust server: profile-guided optimization

Both Rust-server distributions are now built with profile-guided optimization
(PGO) on top of the existing thin-LTO + mimalloc build. A profile collected by
running the six-workload benchmark against an instrumented build teaches the
compiler which branches and call sites are hot, and the final build is optimized
around them. Measured ~12–19% faster on the write and aggregate paths over
LTO alone — moving single-client `aggregate_group` to `mongod` parity and
`aggregate_multistage` from ~2.0× to ~1.7× of `mongod`, with writes now beating
standalone `mongod`. Read-scan paths are unchanged (they were already at the
wire floor). Behaviour is identical — this is a build-time optimization only.

The two distributions get PGO differently: the **standalone `secantusd-rs`
binary** is built two-stage per architecture in its release workflow (an
on-target profile), while the **wheel-embedded `_secantus_server` extension**
uses a committed profile (`crates/pgo/_secantus_server.profdata.tar.gz`,
regenerated with `invoke rust-pgo-refresh`) so an ordinary `pip install` build
picks it up with no extra tooling. The profile is only a hint — unmatched
functions are ignored — so it never blocks a build and a stale profile is safe.

#### Added
- `invoke rust-pgo-refresh` regenerates the committed PGO profile for the
  embedded extension (instrument → run the benchmark workloads → merge → commit
  → rebuild). Needs `rustup component add llvm-tools-preview`.
- `crates/pgo/_secantus_server.profdata.tar.gz`: the committed, sparse PGO
  profile the wheel build consumes.

#### Changed
- Rust server: the embedded `_secantus_server` extension and the standalone
  `secantusd-rs` binary are built with PGO. The extension build (CMake) applies
  the committed profile via `-Cprofile-use`; `SECANTUS_PGO_DISABLE=1` turns it
  off and `SECANTUS_PGO_GENERATE=<dir>` switches to instrumentation.

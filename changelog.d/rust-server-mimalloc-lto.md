### Rust server: a faster allocator and link-time optimization

The Rust server now uses the mimalloc allocator and builds its release
artifacts with thin LTO and a single codegen unit. Profiling had shown that
`malloc`/`realloc`/`free` churn from BSON materialization was among the top CPU
costs on every write and aggregate path; swapping the allocator addresses that
directly, and cross-crate inlining from LTO adds a smaller further gain. On the
six-workload benchmark this moves single-client inserts, updates, and deletes to
roughly `mongod` parity and cuts aggregation time by about a quarter, with no
change to behaviour — it is an allocator and compiler-optimization change only.

#### Changed
- Rust server: mimalloc is the global allocator for the embedded
  `_secantus_server` extension and the standalone `secantusd-rs` binary.
- Rust server: release builds of the embedded extension and the binary use
  `lto = "thin"` and `codegen-units = 1`. Measured single-client gains vs the
  previous build (mongod-normalized): update / delete / aggregate ~−22–25%,
  insert ~−14% (allocator is the dominant lever; LTO is roughly additive on
  top). Raw-scan reads (`find`, indexed range) are unchanged — they already
  serve replies as spliced raw bytes with little server-side allocation.

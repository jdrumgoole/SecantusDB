### WiredTiger builds from a crate

The Rust servers can now build WiredTiger from bundled source, the first step
towards publishing them on crates.io.

#### Added

- `crates/secantus-wiredtiger-sys`: builds the same WiredTiger as the wheel
  (mongodb-7.0.33 with SecantusDB's patches) as a static library with CMake and
  a C compiler, with zlib and lz4 linked statically. No Python is needed.
- `secantus-wt` has a `bundled` feature that uses it. It is off by default, so
  existing builds still link the prebuilt WiredTiger.
- `./inv wt-sys-refresh` regenerates the bundled source. A test fails if the
  source drifts from `vendor/wiredtiger` and the patch scripts.

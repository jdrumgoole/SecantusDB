### `./inv rust-wt-build` recovers from a build directory left by an older checkout

The Rust-side WiredTiger build moved its source from `vendor/wiredtiger` to a
patched copy on 2026-10-07. A checkout that had built before then failed at
the CMake configure step with "the source ... does not match the source ...
used to generate cache", and stayed broken until `build/rust-wt/wt-build` was
deleted by hand.

#### Fixed

- The build now removes a WiredTiger build directory whose CMake cache names a
  different source tree, and rebuilds.
- The 0.7.0b0 changelog entry and its blog post gave `cargo install
  secantus-mdb` without `--version`, which installs nothing. They now name the
  version.

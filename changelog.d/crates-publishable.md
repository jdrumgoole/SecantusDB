### The MongoDB-side Rust crates are publishable

The ten crates behind the Rust MongoDB server now package for crates.io, and a
CI gate keeps them that way.

#### Changed

- The `secantusdb` crate is now named `secantus-mdb`. Its directory, the
  `secantusd-rs` binary and the `secantusdb-v*` release tags are unchanged.
- `secantus-wt` now builds through the bundled WiredTiger by default. The
  `SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB` override still links a prebuilt
  WiredTiger, which is how the wheel and the release binaries build. zlib and
  lz4 are now linked statically from bundled sources everywhere.
- Regenerating the bindings with bindgen is now opt-in (`--features bindgen`).
  `./inv rust-wt-test` turns it on, so the drift check still runs in CI.
- `secantus-wiredtiger-sys` joins the MongoDB server's version line.

#### Added

- Every one of the ten crates has crates.io metadata, a README, and exact
  `=version` pins on its sibling crates. The internal crates say they carry no
  semver promise.
- `secantusd-rs --version` prints `source: crates.io` for a build from a
  packaged crate, which has no git tree to stamp.
- docs.rs can document the crates without compiling WiredTiger.
- `scripts/crates_package_check.py` packages all ten crates together and builds
  each one from its own tarball. The new `crates-package.yml` workflow runs it
  on every PR that touches `crates/`.

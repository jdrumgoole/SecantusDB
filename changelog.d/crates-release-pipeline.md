### Rust release tooling for crates.io

A Rust release now bumps, checks and publishes the crates in one consistent
path.

#### Added

- `./inv rust-version-bump --to <version>` bumps the Rust MongoDB server's
  lockstep version. It rewrites every version, every exact `=` pin between the
  crates, and every `Cargo.lock` that records one. It then fails if the old
  version survives anywhere or a lockfile no longer resolves `--locked`, which
  the release builds require.
- `cargo binstall secantus-mdb` installs the prebuilt, PGO-optimised
  `secantusd-rs` from the GitHub release instead of compiling WiredTiger.

#### Changed

- The crates.io publish step publishes one crate at a time and skips any
  version that is already published. A run that failed part way is fixed by
  re-running it on the same tag.

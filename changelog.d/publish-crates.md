### The Rust MongoDB server publishes to crates.io on release

A `secantusdb-v*` release tag now also publishes the Rust MongoDB server's
crates to crates.io, with no stored token.

#### Added

- `.github/workflows/publish-crates.yml`:
  - It refuses a tag that disagrees with the crate version.
  - It packages the ten crates and builds each one from its tarball.
  - It publishes them in dependency order with `cargo publish --workspace`.
  - It authenticates by crates.io trusted publishing, in a `crates-io`
    environment that accepts only release tags.
  - A manual run defaults to a dry run.

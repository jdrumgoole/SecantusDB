### A Rust version bump now moves the install commands in the docs with it

The README, the docs and the crate READMEs print `cargo install secantus-mdb
--version <ver>` with the version spelled out, because every release so far is
a pre-release and a bare `cargo install` picks the `0.0.0` placeholder. Nothing
moved those versions when the crates moved, so the next Rust release would have
left every page telling readers to install the previous one.

#### Changed

- `./inv rust-version-bump` rewrites the version in every documented `cargo
  install secantus-mdb --version …` and `secantus-mdb = "…"` (and the
  `secantus-pg` forms under `--line pg`), and fails if any page still names
  another version. A version mentioned in passing is left alone.
- A test fails when a documented install command names a version other than
  the one the crates carry.

### The `cargo install` instructions now work

Every place that told you how to install a Rust server from crates.io printed
`cargo install secantus-mdb`, and that command fails. Both crates carry a
`0.0.0` name-reservation release and every real release is a pre-release, so
cargo picks `0.0.0` and stops with "there is nothing to install". With a
version it works: `cargo install secantus-mdb --version 0.5.3-beta.173`, and
`cargo install secantus-pg --version 0.1.0-beta.3` for the Rust PostgreSQL
server.

#### Fixed

- The README, both docs trees, the two crates' READMEs and the site's Rust
  MongoDB page give the install command with `--version`.
- The README now says that both Rust servers install from crates.io; it still
  said the PostgreSQL one was not published.
- A test fails if a published page prints the bare command again.

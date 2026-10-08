### `secantus-pg` is on crates.io

The Rust PostgreSQL server's first crates.io release is `0.1.0-beta.3`:
`cargo install secantus-pg --version 0.1.0-beta.3` builds `secantusd-pg` and
the WiredTiger it links, and `secantus_pg::PgServer` embeds it in a Rust test.
It shipped with `secantus-mdb 0.5.3-beta.173`, which it pins, so a crates.io
build carries the same storage and WiredTiger as the released binaries.

#### Changed

- The site's PostgreSQL page offers `cargo install secantus-pg` and links the
  `0.1.0-beta.3` binaries; the MongoDB page links `0.5.3-beta.173`.

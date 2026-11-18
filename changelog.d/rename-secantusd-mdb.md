### The Rust MongoDB server binary is now `secantusd-mdb`

The Rust MongoDB server's command was `secantusd-rs`, named for the language
it is written in, while the PostgreSQL server's is `secantusd-pg`, named for
the protocol it speaks. The MongoDB server is now `secantusd-mdb`, matching its
`secantus-mdb` crate and its sibling. Its flags and behaviour are unchanged.

**This is a breaking change for scripts.** `secantusd-rs` is gone. Replace it
with `secantusd-mdb` wherever you start the server: in shell scripts, in CI,
and in any path into a release archive.

#### Changed

- `cargo install secantus-mdb`, the `secantusdb-v*` release archives and the
  PyPI wheel all install `secantusd-mdb`. The PostgreSQL server is still
  `secantusd-pg`.
- The PyPI wheel builder still reads archives from releases before this one,
  where the binary was `secantusd-rs`, and installs it as `secantusd-mdb`.

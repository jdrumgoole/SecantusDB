### The Rust PostgreSQL server's crate is linted in CI

`crates/secantus-pg` was built and tested in CI but never run through
`cargo fmt --check` or `cargo clippy`, and six `clippy::drop_non_drop` errors
had accumulated in it. They are fixed and the crate joins the lint step the
other WiredTiger-linked crates already have.

#### Fixed

- Six redundant `drop()` calls on closures, in `grace_join.rs`,
  `stream_join.rs` and `lib.rs`. No behaviour change: a closure with no `Drop`
  releases its borrows at its last use.

#### Changed

- `test.yml`'s `rust-storage` job runs `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings` in `crates/secantus-pg`.

### The Rust servers' locked builds take rustls 0.23.46

rustls 0.23.46 fixes two advisories published on 2026-10-14:
RUSTSEC-2026-0346 (`Debug` output of a client session's values showed TLS
secrets) and RUSTSEC-2026-0349 (an unauthenticated ServerHello could
downgrade a client's later connections to classical key exchange). Both are
in rustls's client, and neither server binary makes a TLS client connection,
so neither was reachable through them. Every committed `Cargo.lock` that carried
0.23.45 now records 0.23.46, so the released binaries and any `--locked`
build use the fixed version. A plain `cargo install` already resolves the
newest 0.23 release.

#### Security

- **rustls 0.23.45 → 0.23.46** in `crates/Cargo.lock` and the lockfiles of
  `secantus-pg`, `secantusdb`, `secantus-server-py` and
  `secantus-storage-adapter`.

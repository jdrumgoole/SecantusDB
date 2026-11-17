### SecantusDB is presented as a real single-node database

The project's positioning no longer frames SecantusDB as a test double or a
tool for running your tests. It is a real single-node database: the MongoDB
and PostgreSQL wire protocols over WiredTiger, run as a standalone daemon,
in a container, or embedded in a program. Single-node scope, the beta status
and the correctness target (`pymongo` and `psycopg` against `mongod` 8.x and
PostgreSQL) are unchanged; tests remain one supported use among others.

#### Changed

- `CLAUDE.md`, `README.md`, the crate READMEs, `docs/`, `docs-rust/` and the
  website sources: the "for your tests / ephemeral CI database / not a
  production-grade emulator" framing is replaced by the real-database framing.
- `secantus-mdb` and `secantus-pg`: the `testing` crates.io keyword is
  replaced by `embedded`.
- `docs/`: Rust-first. The index, installation, quickstart, servers and SQL
  pages now lead with `secantusd-mdb` and `secantusd-pg`. Installation covers
  all three channels: PyPI (`pip install SecantusDB`, the `secantusdb`
  launcher and both prebuilt binaries), crates.io (`cargo install
  secantus-mdb` / `secantus-pg`, and the crates as libraries) and the
  prebuilt archives on GitHub Releases. The Python servers moved to a
  clearly labelled "Deprecated: Python servers" page. Production,
  configuration, authentication, recovery and examples pages use
  `secantusd-mdb` and the `secantusdb` launcher.
- `docs-rust/`: removes the stale claim that the PyPI wheel carries only the
  Python servers, and stops comparing the Rust server to the Python server as
  a reference.
- `secantus-mdb` and `secantus-pg`: the `development-tools::testing` crates.io
  category is dropped.

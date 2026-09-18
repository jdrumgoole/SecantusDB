### `secantusd-pg` can be released

The Rust PostgreSQL server was a headline product with no way to ship it:
`release-binaries.yml` builds the MongoDB binary only, no workflow referenced
the crate, and no tag scheme existed for it.

#### Added

- `release-pg-binaries.yml` — a tag-triggered track for `secantusd-pg`,
  modelled on the MongoDB binary's. Builds static-WiredTiger archives for
  `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`, smokes the exact
  artifact it is about to publish, and attaches `tar.gz` + `.sha256` to a GitHub
  pre-release. Triggered by `secantusd-pg-v<crate-version>` — a third tag
  namespace, verified disjoint from the PyPI `v[0-9]+…` and the MongoDB
  `secantusdb-v*` patterns.
- `tests/test_rust_pg_binary_smoke.py` — psycopg → `secantusd-pg` → WiredTiger
  against the release artifact, including a restart to prove the data is on
  disk. Registered in CI's `pg-oracle` lane, the only lane that builds the
  binary.

#### Fixed

- `secantusd-pg --version` and `--help` now answer. `--version` previously fell
  through to the positional storage-path argument, so the server tried to open a
  WiredTiger database in a directory called `--version` and reported
  `WT_TRY_SALVAGE: database corruption detected`.
- `secantusd-pg` creates its storage directory when missing, which
  `secantusd-rs` already did. A first run against a fresh path failed inside
  WiredTiger with `WiredTiger.lock: handle-open: open: No such file or
  directory` — an alarming answer to pointing it at a new directory.

The PG server is **not** on the MongoDB crates' lockstep version (`0.1.0-beta.0`
against `0.5.3-beta.163`), so its tag carries its own number and the workflow
asserts against `crates/secantus-pgserver/Cargo.toml` alone.

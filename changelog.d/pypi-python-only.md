### The PyPI package is the Python servers; the Rust servers ship as crates

`pip install SecantusDB` now installs the two Python reference servers (MongoDB
and PostgreSQL) and WiredTiger, and nothing else. The Rust servers no longer
ride inside the wheel. The Rust MongoDB server is the `secantus-mdb` crate on
crates.io: `cargo install secantus-mdb` builds it, WiredTiger included, and
puts `secantusd-rs` on your `PATH`. From a Rust test, `secantus_mdb::Server`
starts it in-process. Prebuilt binaries of both Rust servers stay on GitHub
Releases for machines without a Rust toolchain.

Two things drove the split. Bundling the Rust servers multiplied the size of
every wheel. That growth pushed the project past PyPI's 10 GB storage quota,
and `0.6.0b18`'s upload was refused. It also meant a Python user downloaded
two Rust servers they might never run.

#### Changed

- `publish.yml` and `wheels.yml` build the wheel with
  `SECANTUS_BUILD_STORAGE_ENGINE` off, so it carries no `_secantus_server`,
  `_secantus_storage` or `secantusd-rs`.
- **If you used the embedded handle** (`from _secantus_server import
  RustServer`) from a PyPI install, you have two options. You can run
  `secantusd-rs` as a subprocess and point `pymongo` at it, or you can use
  `SecantusDBServer`, which has the same `pymongo` surface. The handle remains
  available in a source build.
- The README, the server comparison, the installation pages and the website
  now give `cargo install secantus-mdb` as the way to get the Rust MongoDB
  server.

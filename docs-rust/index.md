# SecantusDB Rust server

The SecantusDB **Rust server** is SecantusDB's MongoDB database server — wire parsing, command
dispatch, cursors, change streams, the operator engines, and WiredTiger storage, all in Rust with no
Python in the request path. It speaks the MongoDB wire protocol, passes the
unmodified driver-conformance suites (99.5% of pymongo's own tests), and ships
as a single static-WiredTiger binary: `secantusd-mdb`.

It ships three ways: the `secantus-mdb` crate on crates.io, prebuilt archives
on GitHub Releases, and inside the `SecantusDB` wheel on PyPI (which also
carries the PostgreSQL server, `secantusd-pg`):

```bash
pip install SecantusDB     # prebuilt secantusd-mdb + secantusd-pg on PATH, nothing to compile
# or: cargo install secantus-mdb --version 0.5.3-beta.197   (builds WiredTiger)
secantusd-mdb --port 27017 --storage-path ./secantus-data
```

```rust
// Or in-process, from Rust (add `secantus-mdb` as a dependency):
let server = secantus_mdb::Server::start()?;   // temporary store, OS-assigned port
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
```

Prebuilt archives (Linux x86_64, macOS arm64, Windows x86_64) are on GitHub
Releases for machines without a Rust toolchain. See [Installation](installation.md).

Every MongoDB driver connects to it unchanged: it is held to `mongod` 8.x, so
the `OP_MSG` handshake, the commands and the error codes are `mongod`'s, over
the same WiredTiger on-disk engine.

And it is fast: on the nine-workload benchmark the Rust server runs at
**~1.0×–3.2× of real `mongod`** per operation across three runs
(reads and the change-stream drain at 1.0×–1.4×, multi-stage
aggregation at the high end — after a mimalloc allocator, LTO, and profile-guided optimization cut the
BSON-materialization allocation and hot-path branch cost), sustains
**3.3×–3.7× multi-writer scaling fully durable** (monotonic to eight
writers),
measured end-to-end through `pymongo` on
on-disk WiredTiger. Numbers and methodology:
[Benchmark](https://secantusdb.com/docs/benchmark.html) and
[Concurrency](https://secantusdb.com/docs/concurrency.html).

## Where this fits

SecantusDB is two servers: this MongoDB server (`secantusd-mdb`) and a
PostgreSQL server (`secantusd-pg`, beta), on independent version lines. Each
is held to the real product (`mongod`, PostgreSQL). The main documentation,
[secantusdb.com/docs](https://secantusdb.com/docs/index.html), covers both and
the [feature comparison](https://secantusdb.com/docs/feature-comparison.html)
maps the server against real MongoDB, feature by feature. The original Python
servers are fully deprecated.

```{toctree}
:maxdepth: 2
:caption: Contents

installation
running
embedded
security
recovery
conformance
coverage
architecture
releases
```

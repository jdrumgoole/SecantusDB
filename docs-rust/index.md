# SecantusDB Rust server

The SecantusDB **Rust server** is a whole second implementation of the
SecantusDB database — wire parsing, command dispatch, cursors, change
streams, the operator engines, and WiredTiger storage, all in Rust with no
Python in the request path. It speaks the same MongoDB wire protocol as
[the Python server](https://secantusdb.com/docs/), passes the same
unmodified driver-conformance suites (99.5% of pymongo's own tests — level
with the Python server), and ships as a single static-WiredTiger binary:
`secantusd-rs`.

It ships as the `secantus-mdb` crate on crates.io:

```bash
cargo install secantus-mdb --version 0.5.3-beta.174   # builds WiredTiger, installs `secantusd-rs`
secantusd-rs --port 27017 --storage-path ./secantus-data
```

```rust
// Or in-process, from a Rust test (add `secantus-mdb` as a dev-dependency):
let server = secantus_mdb::Server::start()?;   // temporary store, OS-assigned port
let client = mongodb::sync::Client::with_uri_str(server.uri())?;
```

Prebuilt archives (Linux x86_64, macOS arm64, Windows x86_64) are on GitHub
Releases for machines without a Rust toolchain. The Rust server is **not** in
the `SecantusDB` Python wheel, which carries only the Python servers.

Every MongoDB driver that talks to the Python server talks to the Rust
server unchanged — same `OP_MSG` handshake, same commands, same error
codes, same on-disk WiredTiger semantics.

And it is fast: on the nine-workload benchmark the Rust server runs at
**~1.0×–4.1× of real `mongod`** per operation across three runs of the same
build (reads and the change-stream drain at 1.0×–1.4×, multi-stage
aggregation at the high end — after a mimalloc allocator, LTO, and profile-guided optimization cut the
BSON-materialization allocation and hot-path branch cost), sustains
**3.3×–3.7× multi-writer scaling fully durable** (monotonic to eight
writers), and is roughly 1.7×–14.8× faster than the Python server
workload-for-workload — measured end-to-end through `pymongo` on
on-disk WiredTiger. Numbers and methodology:
[Benchmark](https://secantusdb.com/docs/benchmark.html) and
[Concurrency](https://secantusdb.com/docs/concurrency.html).

## Where this fits

SecantusDB ships **two separate servers** on independent version lines. The
Python server is the conformance reference and the default choice; the Rust
server is the same database built for speed and dependency-free standalone
deployment. The full decision guide is
[The two servers](https://secantusdb.com/docs/servers.html), and the
[feature comparison](https://secantusdb.com/docs/feature-comparison.html)
maps both servers against real MongoDB, feature by feature.

```{toctree}
:maxdepth: 2
:caption: Contents

installation
running
embedded
security
recovery
conformance
architecture
releases
```

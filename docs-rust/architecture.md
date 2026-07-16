# Architecture

The Rust server is a Cargo workspace under `crates/`, split so the pure
engines never depend on Python **or** WiredTiger:

| Crate | What it is |
| --- | --- |
| `secantus-core` | The pure-Rust operator engines: query matcher, update engine, aggregation pipeline + expression evaluator, projection, sort keys, geo, collation. No PyO3, no WiredTiger — pure functions over BSON. |
| `secantus-storage` | The full WiredTiger-backed `Storage`: document/index/oplog tables, change-stream projection, transactions, backup/PITR archive + replay. |
| `secantus-wt` | The WiredTiger FFI bindings (bindgen over the vendored C library). |
| `secantus-server` | The server proper: wire protocol (`OP_MSG` / legacy handshake), command dispatch, cursors, auth (`secantus-auth`), TLS accept loop (rustls), CLI argument/config parsing. |
| `secantus-auth` | SCRAM-SHA-1/256 mechanics, X509 DN matching, RBAC role resolution. |
| `secantusdb` | The standalone binary crate — `secantusd-rs`. Its own workspace (it links WiredTiger); builds the vendored WT and statically links it. |
| `secantus-core-py`, `secantus-server-py`, `secantus-storage-py` | Thin PyO3 binding crates: `_secantus_core` (the parity-test vehicle), `_secantus_server` (the embedded `RustServer` handle), `_secantus_storage`. Python is only ever a launcher — never in the request path. |

The WiredTiger-linked crates are excluded from the clean workspace so
`cargo fmt` / `clippy` / `test` run fast and toolchain-light; the WT crates
build inside the wheel (CMake + scikit-build-core) or the standalone binary
build.

## Parity with the Python server

Every Rust operator engine is pinned byte-for-byte to its pure-Python
counterpart by parity test suites (curated corpora plus randomized fuzz).
Where an exact reproduction isn't achievable — Python-`re` regex semantics,
some collation and Decimal128 edges — the Rust engine **defers**: it
rejects the construct with a clean error rather than return a subtly
different answer. That discipline is why the conformance numbers of the two
servers track each other so closely.

## Storage

Same model as the Python server, same on-disk schema: documents stored as
opaque BSON blobs keyed by byte-sortable `_id` encodings, secondary indexes
over typed sort-key columns, a natural-order (insertion) index, and the
oplog/pre-image tables that back change streams and point-in-time recovery.
A data directory written by one server opens in the other.

## Versioning

All crates carry one lockstep version (`0.MAJOR.PATCH-beta.N`, SemVer
pre-release) that advances independently of the PyPI package's version.
The canonical embedded value is `secantus_server::VERSION`, surfaced as
`buildInfo.secantusVersion` over the wire, `secantusd-rs --version` on the
CLI, and `RustServer.version` on the embedded handle. See
[Releases](releases.md).

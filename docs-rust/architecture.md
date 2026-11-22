# Architecture

The Rust server is a Cargo workspace under `crates/`, split so the pure
engines never depend on WiredTiger. The full layer map, for both servers, is
in the main tree's [Architecture](https://secantusdb.com/docs/architecture.html)
page; the crates the MongoDB server is built from:

| Crate | What it is |
| --- | --- |
| `secantus-core` | The pure-Rust operator engines: query matcher, update engine, aggregation pipeline + expression evaluator, projection, sort keys, geo, collation. No WiredTiger — pure functions over BSON. |
| `secantus-storage` | The full WiredTiger-backed `Storage`: document/index/oplog tables, change-stream projection, transactions, backup/PITR archive + replay. |
| `secantus-wt` | The WiredTiger FFI bindings (bindgen over the vendored C library). |
| `secantus-server` | The server proper: wire protocol (`OP_MSG` / legacy handshake), command dispatch, cursors, auth (`secantus-auth`), TLS accept loop (rustls), CLI argument/config parsing. |
| `secantus-auth` | SCRAM-SHA-1/256 mechanics, X509 DN matching, RBAC role resolution. |
| `secantusdb` | The standalone binary crate — `secantusd-mdb`. Its own workspace (it links WiredTiger); builds the vendored WT and statically links it. |
| `secantus-core-py`, `secantus-server-py`, `secantus-storage-py` | Thin PyO3 binding crates (`_secantus_core`, `_secantus_server`, `_secantus_storage`) used by the repository's own tests in a source build. They are not shipped by `pip install SecantusDB`, and Python is never in the request path. |

The WiredTiger-linked crates are excluded from the clean workspace so
`cargo fmt` / `clippy` / `test` run fast and toolchain-light; the WT crates
are built from their own directories (CMake builds the vendored WiredTiger).

## Engines and `mongod`

Collation compares strings through ICU (`secantus-icu`, the ICU `mongod`
links), and the engines are checked against `mongod` directly. Where an exact
reproduction is not achievable, an engine **defers**: it rejects the construct
with a clean error rather than return a subtly different answer.

## Storage

The store is `mongod` 8.2's own layout on WiredTiger (store format 10), so
`mongod` opens a store this server wrote and this server opens one `mongod`
wrote, as a hand-off and never at once. `_mdb_catalog` lists every collection
and index with its ident; each collection is a table of its own, holding its
documents as opaque BSON blobs keyed by RecordId (insertion order), and each
index is a table of its own, keyed by `mongod`'s KeyString. The oplog is
`local.oplog.rs`, with the pre-image table, and backs change streams and
point-in-time recovery; sizes are kept in `sizeStorer`, and the log is in
`journal/`. Dropping or renaming a collection changes the catalog only. A
store in an older SecantusDB layout is refused by name, with dump-and-restore
advice.

## Versioning

All crates carry one lockstep version (`0.MAJOR.PATCH-beta.N`, SemVer
pre-release) that advances independently of the PostgreSQL server's.
The canonical embedded value is `secantus_server::VERSION`, surfaced as
`buildInfo.secantusVersion` over the wire, and `secantusd-mdb --version` on the
CLI. See
[Releases](releases.md).
